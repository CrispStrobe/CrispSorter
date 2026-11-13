#!/usr/bin/env python3
"""Check signed OCR draft packages; keep optional, minimized diagnostics."""
import json
import hashlib
import os
import plistlib
from pathlib import Path
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import zipfile

from check_desktop_release_assets import sha
from mobile_draft_safe_cli import SafeArgumentParser, run as safe_run
from check_mobile_release_profile import profile
import ocr_mobile_build_receipt as native
from prepare_signed_mobile_ocr_draft import RUNTIME_LOCK
from sign_android_apk import signer_path


def check_native(project, staging, target, app):
    """Use existing package checks without source-tree/report admission checks."""
    runtime_path = staging / 'runtime/runtime-build.json'
    runtime = json.loads(runtime_path.read_text())
    lock = json.loads((project / RUNTIME_LOCK).read_text())
    if runtime.get('target') != target or any(runtime.get(k) != lock[k] for k in ['source_commit', 'ggml_commit']):
        raise ValueError('Packaged native runtime target or configured runtime differs')
    if target == 'android-arm64':
        native.android_identity(app, runtime_path.parent, runtime)
        platform = 'android'
    else:
        native.bundle_identity(app, 'iphoneos')
        platform = 'ios-device'
    native.ort_identity(project, platform, app, staging / 'ort/ort-runtime.json', target)
    native.verify_native_provenance(app, platform, runtime_path.parent, runtime, lock)


def check_android(project, staging, artifact, version):
    signer = signer_path(None)
    verified = subprocess.check_output([signer, 'verify', '--verbose', '--print-certs', str(artifact)],
                                       text=True, timeout=30, stderr=subprocess.PIPE)
    # Compare the actual APK signer with the configured keystore certificate;
    # no generated signing report is needed and passwords never enter argv.
    certificate = subprocess.check_output(['keytool', '-exportcert', '-keystore', os.environ['ANDROID_KEYSTORE'],
        '-storepass:env', 'ANDROID_KEYSTORE_PASSWORD', '-alias', os.environ['ANDROID_KEY_ALIAS']],
        timeout=30, stderr=subprocess.PIPE)
    expected = hashlib.sha256(certificate).hexdigest()
    signers = re.findall(r'(?m)^Signer #[0-9]+ certificate SHA-256 digest: ([0-9a-fA-F]{64})\s*$', verified)
    if len(signers) != 1 or signers[0].lower() != expected:
        raise ValueError('APK signer differs from the configured Android certificate')
    aapt = Path(signer).with_name('aapt')
    metadata = subprocess.check_output([str(aapt), 'dump', 'badging', str(artifact)], text=True, timeout=30)
    package = re.search(r"(?m)^package: name='([^']+)'[^\n]* versionName='([^']+)'", metadata)
    if not package or package[1] != native.PACKAGE or package[2] != version:
        raise ValueError('APK application identifier or version differs from the build configuration')
    check_native(project, staging, 'android-arm64', artifact)


def check_ios(project, staging, artifact, version):
    with zipfile.ZipFile(artifact) as archive:
        entries = archive.infolist()
        if (len(entries) > 20_000 or sum(x.file_size for x in entries) > 2 * 1024**3
                or len({x.filename for x in entries}) != len(entries)
                or any(stat.S_ISLNK(x.external_attr >> 16) or Path(x.filename).is_absolute()
                       or '..' in Path(x.filename).parts or '\\' in x.filename for x in entries)):
            raise ValueError('Unexpected signed IPA archive contents')
    with tempfile.TemporaryDirectory(prefix='signed-ipa-', dir=staging, ignore_cleanup_errors=True) as temporary:
        extracted = Path(temporary)
        subprocess.run(['unzip', '-q', str(artifact), '-d', str(extracted)], check=True)
        apps = list((extracted / 'Payload').glob('*.app'))
        if len(apps) != 1 or not (apps[0] / 'embedded.mobileprovision').is_file():
            raise ValueError('Signed OCR IPA requires exactly one provisioned device application')
        app = apps[0]
        info = plistlib.loads((app / 'Info.plist').read_bytes())
        if info.get('CFBundleShortVersionString') != version:
            raise ValueError('Exported signed IPA version differs from the build configuration')
        subprocess.run(['codesign', '--verify', '--deep', '--strict', str(app)], check=True)
        check_native(project, staging, 'ios-arm64', app)


def optional_output(result, report):
    """Only minimized JSON is optional; the signed package is a product output."""
    try:
        if result.is_symlink():
            return
        result.mkdir(exist_ok=True)
        for name in ['signed-draft.json', 'failure.json']:
            (result / name).unlink(missing_ok=True)
        name = 'signed-draft.json' if report['validation_status'] == 'passed' else 'failure.json'
        (result / name).write_text(json.dumps(report, indent=2) + '\n')
    except OSError:
        print('Optional signed draft diagnostics could not be retained', file=sys.stderr)


def retain_package(result, artifact, filename):
    if result.is_symlink():
        raise ValueError('Package output directory must be regular')
    result.mkdir(exist_ok=True)
    destination = result / filename
    if destination.is_symlink():
        raise ValueError('Package output must be a regular file')
    # Overwrite the same product on a normal retry and avoid uploading packages
    # left by another version. Never remove the current input package.
    for previous in result.iterdir():
        if previous.suffix in ('.apk', '.ipa') and previous.resolve() != artifact:
            previous.unlink()
    if destination.resolve() != artifact:
        shutil.copy2(artifact, destination)
    if not destination.is_file() or sha(destination) != sha(artifact):
        raise ValueError('Final signed package was not retained intact')


def record_draft(project, staging, target, artifact):
    profile('ocr-draft')
    if os.environ.get('CRISPSORTER_RELEASE_SELECTOR') != {'android-arm64': 'android_ocr_draft', 'ios-arm64': 'ios_ocr_draft'}.get(target):
        raise ValueError('Draft target differs from its explicit selector')
    project, staging, artifact = project.resolve(), staging.resolve(), artifact.resolve()
    version = json.loads((project / 'src-tauri/tauri.conf.json').read_text())['version']
    if not isinstance(version, str) or not re.fullmatch(r'[0-9A-Za-z.-]{1,80}', version):
        raise ValueError('Invalid application version')
    suffix = '.apk' if target == 'android-arm64' else '.ipa'
    report = {'schema': 'crispsorter-signed-ocr-draft-checks-v1', 'target': target, 'version': version,
              'filename': f'CrispSorter_{version}_{target}-ocr-draft-unqualified{suffix}',
              'validation_status': 'failed', 'qualification': 'Not established; no device or OCR quality execution implied',
              'distribution': 'Workflow artifact only; no release or App Store upload'}
    try:
        if target == 'android-arm64':
            check_android(project, staging, artifact, version)
        else:
            check_ios(project, staging, artifact, version)
        retain_package(staging / 'artifact', artifact, report['filename'])
        report.update(validation_status='passed', sha256=sha(artifact))
    except Exception:
        optional_output(staging / 'artifact', report)
        raise
    optional_output(staging / 'artifact', report)
    return report


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
