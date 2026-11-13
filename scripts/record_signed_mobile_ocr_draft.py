#!/usr/bin/env python3
"""Verify final signed draft provenance without distributing or qualifying it."""
import hashlib
import json
import os
import plistlib
from pathlib import Path
import shutil
import stat
import subprocess
import sys
import zipfile

import ios_draft_retention_schema as retention_schema
from retain_ios_ocr_draft_inputs import ORT_FILES
from check_desktop_release_assets import sha
from mobile_draft_safe_cli import SafeArgumentParser, run as safe_run
from check_mobile_release_profile import profile
from ocr_mobile_build_receipt import record
from prepare_signed_mobile_ocr_draft import RUNTIME_LOCK
from sign_android_apk import signer_path


def record_draft(project, staging, target, artifact):
    description = profile('ocr-draft')
    if os.environ.get('CRISPSORTER_RELEASE_SELECTOR') != {'android-arm64': 'android_ocr_draft', 'ios-arm64': 'ios_ocr_draft'}.get(target):
        raise ValueError('Draft target differs from its explicit selector')
    project, staging, artifact = project.resolve(), staging.resolve(), artifact.resolve()
    preparation = json.loads((staging / 'preparation.json').read_text())
    if (preparation.get('target') != target or preparation.get('runtime_lock') != RUNTIME_LOCK
            or preparation.get('runtime_lock_sha256') != hashlib.sha256((project / RUNTIME_LOCK).read_bytes()).hexdigest()):
        raise ValueError('Draft preparation identity changed before final-artifact validation')
    if not artifact.is_relative_to(staging) and not artifact.is_relative_to(project / 'build'):
        raise ValueError('Keep final signed draft artifacts in staging or ignored build output')
    result = staging / 'artifact'
    if result.exists():
        raise ValueError('Draft artifact evidence must not replace an earlier record')
    result.mkdir()
    version = json.loads((project / 'src-tauri/tauri.conf.json').read_text())['version']
    if preparation.get('version') != version:
        raise ValueError('Application version changed after draft preparation')
    suffix = '.apk' if target == 'android-arm64' else '.ipa'
    retained = result / f'CrispSorter_{version}_{target}-ocr-draft-unqualified{suffix}'
    shutil.copy2(artifact, retained)
    return validate(project, staging, target, artifact, preparation, description, result, version, retained)


def validate(project, staging, target, artifact, preparation, description, result, version, retained):
    try:
        return validate_inner(project, staging, target, artifact, preparation, description, result, version, retained)
    except Exception as error:
        (result / 'failure.json').write_text(json.dumps({
            'schema': 'crispsorter-signed-ocr-draft-failure-v1', 'validation_status': 'failed',
            'qualification': 'Not established', 'error_type': type(error).__name__,
            'error': 'Final draft validation failed; consult the original workflow step diagnostics'}, indent=2) + '\n')
        raise


def validate_inner(project, staging, target, artifact, preparation, description, result, version, retained):
    ios_metadata = (retention_schema.validated_ios_metadata(project, staging, description, ORT_FILES)
                    if target == 'ios-arm64' else None)
    if target == 'android-arm64':
        signing = json.loads(artifact.with_suffix('.signing.json').read_text())
        if (signing.get('signing') != 'provided production certificate' or signing.get('sha256') != sha(artifact)
                or signing.get('filename') != artifact.name):
            raise ValueError('Signed OCR draft requires matching retained production Android signing evidence')
        subprocess.run([signer_path(None), 'verify', '--verbose', '--print-certs', str(artifact)], check=True)
        app, platform = artifact, 'android'
        shutil.copy2(artifact.with_suffix('.signing.json'), result / 'android.signing.json')
    elif target == 'ios-arm64':
        extracted = staging / 'signed-ipa'
        # This is the locally exported IPA; still reject unsafe archive paths and
        # unexpectedly large metadata before extracting it for codesign/provenance.
        with zipfile.ZipFile(artifact) as archive:
            entries = archive.infolist()
            if (len(entries) > 20_000 or sum(x.file_size for x in entries) > 2 * 1024**3
                    or len({x.filename for x in entries}) != len(entries)
                    or any(stat.S_ISLNK(x.external_attr >> 16) or Path(x.filename).is_absolute() or '..' in Path(x.filename).parts or '\\' in x.filename for x in entries)):
                raise ValueError('Unexpected signed IPA archive contents')
        subprocess.run(['unzip', '-q', str(artifact), '-d', str(extracted)], check=True)
        apps = list((extracted / 'Payload').glob('*.app'))
        if len(apps) != 1 or not (apps[0] / 'embedded.mobileprovision').is_file():
            raise ValueError('Signed OCR IPA requires exactly one provisioned device application')
        app, platform = apps[0], 'ios-device'
        info = plistlib.loads((app / 'Info.plist').read_bytes())
        if info.get('CFBundleShortVersionString') != version:
            raise ValueError('Exported signed IPA version differs from the source version')
        subprocess.run(['codesign', '--verify', '--deep', '--strict', str(app)], check=True)
    else:
        raise ValueError('Unsupported signed draft target')
    build = record(project, platform, app, staging / 'runtime/runtime-build.json', staging / 'ort/ort-runtime.json', RUNTIME_LOCK)
    if build['sorter_commit'] != preparation['app_source'] or build['crispembed_commit'] != preparation['production_source']:
        raise ValueError('Final signed application differs from prepared source/runtime identity')
    if ios_metadata is not None and retention_schema.validated_ios_metadata(project, staging, description, ORT_FILES) != ios_metadata:
        raise ValueError('Original iOS metadata changed during final provenance validation')
    build_path = result / 'build-receipt.json'
    build_path.write_text(json.dumps(build, indent=2) + '\n')
    final = {'schema': 'crispsorter-signed-ocr-draft-v1', 'app_source': build['sorter_commit'],
             'target': target, 'version': version, 'filename': retained.name, 'sha256': sha(retained),
             'build_receipt_sha256': sha(build_path), 'profile': description, 'validation_status': 'passed',
             'distribution': 'Workflow artifact only; not uploaded to App Store Connect or a GitHub release',
             'qualification': 'Not established; no application/device or OCR quality execution implied'}
    # Keep only explicit build metadata, never keychains, certificates or signing secrets.
    for name, source in [('preparation.json', staging / 'preparation.json'), ('layout.json', staging / 'layout.json'),
                         ('runtime-build.json', staging / 'runtime/runtime-build.json'), ('ort-runtime.json', staging / 'ort/ort-runtime.json')]:
        if ios_metadata is not None:
            (result / name).write_bytes(ios_metadata[name])
        else:
            shutil.copy2(source, result / name)
    (result / 'signed-draft.json').write_text(json.dumps(final, indent=2) + '\n')
    return final


def main():
    parser = SafeArgumentParser(description=__doc__)
    parser.add_argument('--project', type=Path, default=Path.cwd())
    parser.add_argument('--staging', type=Path, required=True)
    parser.add_argument('--target', choices=['android-arm64', 'ios-arm64'], required=True)
    parser.add_argument('--artifact', type=Path, required=True)
    args = parser.parse_args()
    print(json.dumps(record_draft(args.project, args.staging, args.target, args.artifact), indent=2))


if __name__ == '__main__':
    sys.exit(safe_run(main))
