#!/usr/bin/env python3
"""Select the configured version's existing IPA for upload or assignment retry.

Selection reads GitHub only. The workflow verifies the actual signature and
checks App Store Connect before uploading; no source receipt is required.
"""
import argparse
from datetime import datetime, timezone
import json
from pathlib import Path
import plistlib
import re
import shutil
import subprocess
import sys
import tempfile
import zipfile

from check_testflight_ipa import check_ipa


def gh(*args):
    return subprocess.check_output(['gh', *args], text=True)


def resume_timestamp(value):
    """An optional operator filter, never required to resume the exact build."""
    if not isinstance(value, str) or not re.fullmatch(r'[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z', value):
        raise ValueError('Upload timestamp must use YYYY-MM-DDTHH:MM:SSZ')
    timestamp = datetime.strptime(value, '%Y-%m-%dT%H:%M:%SZ').replace(tzinfo=timezone.utc)
    if timestamp > datetime.now(timezone.utc):
        raise ValueError('Upload timestamp cannot be in the future')
    return value


def prepare(repo, project, output, publish=False, mac_appstore=False, mode='upload', uploaded_after=None):
    if publish is not False or mac_appstore is not False:
        raise ValueError('TestFlight selection requires publish=false and mac_appstore=false')
    if mode not in ('upload', 'resume'):
        raise ValueError('TestFlight mode must be upload or resume')
    if uploaded_after:
        if mode != 'resume':
            raise ValueError('An upload timestamp is accepted only in resume mode')
        uploaded_after = resume_timestamp(uploaded_after)
    version = json.loads((Path(project) / 'src-tauri/tauri.conf.json').read_text())['version']
    if not isinstance(version, str) or not re.fullmatch(r'(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)', version):
        raise ValueError('Configured IPA version must be numeric major.minor.patch')
    tag = 'v' + version
    filename = f'CrispSorter_{version}_ios-arm64.ipa'
    release = json.loads(gh('release', 'view', tag, '--repo', repo, '--json', 'assets'))
    assets = release.get('assets', [])
    if not isinstance(assets, list) or sum(isinstance(asset, dict) and asset.get('name') == filename for asset in assets) != 1:
        raise ValueError('Release must contain exactly one IPA for the configured version')
    output = Path(output).resolve()
    with tempfile.TemporaryDirectory(prefix='crispsorter-testflight-') as directory:
        work = Path(directory)
        gh('release', 'download', tag, '--repo', repo, '--pattern', filename, '--dir', str(work))
        ipa = work / filename
        identity = check_ipa(ipa, version)
        output.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(ipa, output / filename)
    return {**identity, 'mode': mode, 'tag': tag, 'ipa': str(output / filename),
            'uploaded_after': uploaded_after or ''}


def write_environment(selection, destination):
    values = {'IPA_VERSION': selection['version'], 'IPA_FILE': selection['ipa'], 'IPA_SHA256': selection['sha256']}
    if selection.get('mode') == 'resume':
        values['UPLOADED_AFTER'] = selection.get('uploaded_after', '')
    if any('\n' in value or '\r' in value for value in values.values()):
        raise ValueError('Invalid upload selection environment value')
    with Path(destination).open('a') as stream:
        for key, value in values.items():
            stream.write(f'{key}={value}\n')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repo', required=True)
    parser.add_argument('--project', type=Path, default=Path(__file__).resolve().parent.parent)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--publish', choices=['true', 'false'], required=True)
    parser.add_argument('--mac-appstore', choices=['true', 'false'], required=True)
    parser.add_argument('--mode', choices=['upload', 'resume'], default='upload')
    parser.add_argument('--uploaded-after', default=None, help='Optional UTC filter for assignment retry')
    parser.add_argument('--github-env', type=Path)
    args = parser.parse_args()
    try:
        selection = prepare(args.repo, args.project, args.output, args.publish == 'true',
                            args.mac_appstore == 'true', args.mode, args.uploaded_after)
        if args.github_env:
            write_environment(selection, args.github_env)
    except (OSError, RuntimeError, ValueError, KeyError, TypeError, subprocess.SubprocessError,
            zipfile.BadZipFile, plistlib.InvalidFileException) as error:
        print(f'::error::{error}', file=sys.stderr)
        sys.exit(1)
    print(json.dumps(selection, sort_keys=True))


if __name__ == '__main__':
    main()
