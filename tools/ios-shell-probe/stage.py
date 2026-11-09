#!/usr/bin/env python3
"""Validate and stage one immutable simulator app without rebuilding it."""
import argparse
import hashlib
import json
import shutil
import stat
import sys
import zipfile
from pathlib import Path, PurePosixPath

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'scripts'))
from ocr_mobile_build_receipt import bundle_identity


def digest(path):
    value = hashlib.sha256()
    with path.open('rb') as source:
        for block in iter(lambda: source.read(64 * 1024), b''):
            value.update(block)
    return value.hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--archive', type=Path, required=True)
    parser.add_argument('--metadata', type=Path)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    pin_path = Path(__file__).with_name('artifact.json')
    pin = json.loads(pin_path.read_text())
    archive = args.archive.resolve()
    if args.archive.is_symlink() or not archive.is_file():
        raise ValueError('The retained artifact must be a regular ZIP file')
    if archive.stat().st_size != pin['artifact_bytes'] or digest(archive) != pin['artifact_sha256']:
        raise ValueError('The retained GitHub artifact differs from its pinned size or SHA-256')
    if args.metadata:
        metadata = json.loads(args.metadata.read_text())
        if (metadata['id'] != pin['artifact_id'] or metadata['name'] != pin['artifact_name']
                or metadata['size_in_bytes'] != pin['artifact_bytes']
                or metadata['digest'] != 'sha256:' + pin['artifact_sha256']
                or metadata['workflow_run']['id'] != pin['run_id']
                or metadata['workflow_run']['head_sha'] != pin['application_source']):
            raise ValueError('GitHub artifact metadata does not match the pinned source build')
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    nested = output / 'CrispSorter-ocr-simulator.zip'
    with zipfile.ZipFile(archive) as outer:
        if len(set(outer.namelist())) != len(outer.namelist()):
            raise ValueError('Duplicate outer artifact members are not allowed')
        if outer.getinfo(pin['bundle_archive_member']).file_size != pin['bundle_archive_bytes']:
            raise ValueError('Unexpected simulator bundle archive size')
        receipt_info = outer.getinfo(pin['build_receipt_member'])
        if not 0 < receipt_info.file_size <= 256 * 1024:
            raise ValueError('Build receipt exceeds its bounded size')
        with outer.open(pin['bundle_archive_member']) as source, nested.open('xb') as destination:
            shutil.copyfileobj(source, destination, 64 * 1024)
        receipt_data = outer.read(receipt_info)
    if digest(nested) != pin['bundle_archive_sha256']:
        raise ValueError('The simulator bundle archive differs from its pinned SHA-256')
    receipt = json.loads(receipt_data)
    if (receipt['sorter_commit'] != pin['application_source']
            or receipt['package'] != pin['bundle_identifier']
            or receipt['platform'] != 'ios-simulator'
            or receipt['artifact']['sha256'] != pin['bundle_sha256']
            or receipt['application_source_clean'] is not True):
        raise ValueError('The retained build receipt does not identify the expected application')
    (output / 'build-receipt.json').write_bytes(receipt_data)
    with zipfile.ZipFile(nested) as inner:
        members = inner.infolist()
        if len(members) > 256 or len(set(inner.namelist())) != len(members):
            raise ValueError('Unexpected or duplicate app bundle members')
        if sum(member.file_size for member in members) > 800 * 1024 * 1024:
            raise ValueError('App bundle exceeds its bounded uncompressed size')
        for member in members:
            relative = PurePosixPath(member.filename)
            mode = member.external_attr >> 16
            if (relative.is_absolute() or '..' in relative.parts or not relative.parts
                    or relative.parts[0] != 'CrispSorter.app' or '\\' in member.filename
                    or stat.S_ISLNK(mode) or member.file_size > 600 * 1024 * 1024):
                raise ValueError('Unsafe app bundle member: ' + member.filename)
            target = output.joinpath(*relative.parts)
            if not target.resolve().is_relative_to(output):
                raise ValueError('App bundle member escapes the staging directory')
            if member.is_dir():
                target.mkdir(parents=True, exist_ok=True)
                continue
            if stat.S_IFMT(mode) not in (0, stat.S_IFREG):
                raise ValueError('App bundle member is not a regular file')
            target.parent.mkdir(parents=True, exist_ok=True)
            with inner.open(member) as source, target.open('xb') as destination:
                shutil.copyfileobj(source, destination, 64 * 1024)
            target.chmod(stat.S_IMODE(mode) & 0o777 or 0o644)
    identity = bundle_identity(output / 'CrispSorter.app', 'iphonesimulator')
    if identity != receipt['artifact'] or identity['sha256'] != pin['bundle_sha256']:
        raise ValueError('The staged app bytes differ from the retained build receipt')
    staged = {'schema_version': 1, 'kind': 'ios-shell-probe-input', 'pin': pin,
              'pin_sha256': digest(pin_path), 'build_receipt_sha256': digest(output / 'build-receipt.json'),
              'source_bundle_sha256': identity['sha256'], 'application_rebuilt': False,
              'probe_scope': 'Navigation and accessibility only; no changes to the retained application.'}
    (output / 'input-receipt.json').write_text(json.dumps(staged, indent=2) + '\n')
    print(json.dumps({'status': 'staged', 'bundle_sha256': identity['sha256'], 'files': len(identity['files'])}))


if __name__ == '__main__':
    main()
