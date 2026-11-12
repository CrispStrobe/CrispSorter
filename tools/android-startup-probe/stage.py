#!/usr/bin/env python3
"""Validate and stage one immutable Android application for startup diagnosis."""
import argparse
import hashlib
import json
from pathlib import Path
import stat
import zipfile


def digest(path):
    with path.open('rb') as source:
        return hashlib.file_digest(source, 'sha256').hexdigest()


def stage(archive, metadata, output, pin):
    if archive.is_symlink() or not archive.is_file():
        raise ValueError('Expected a regular retained artifact ZIP')
    if archive.stat().st_size != pin['artifact_bytes'] or digest(archive) != pin['artifact_sha256']:
        raise ValueError('Retained artifact size or SHA-256 differs from the immutable pin')
    if (metadata['id'] != pin['artifact_id'] or metadata['name'] != pin['artifact_name']
            or metadata['size_in_bytes'] != pin['artifact_bytes']
            or metadata['digest'] != 'sha256:' + pin['artifact_sha256']
            or metadata['workflow_run']['id'] != pin['run_id']
            or metadata['workflow_run']['head_sha'] != pin['application_source']):
        raise ValueError('Artifact metadata does not identify the pinned source build')
    output.mkdir(parents=True, exist_ok=False)
    with zipfile.ZipFile(archive) as outer:
        if len(outer.infolist()) > 4096 or len(set(outer.namelist())) != len(outer.infolist()):
            raise ValueError('Unexpected or duplicate artifact members')
        apk_info = outer.getinfo(pin['apk_member'])
        receipt_info = outer.getinfo(pin['build_receipt_member'])
        for info in (apk_info, receipt_info):
            if stat.S_IFMT(info.external_attr >> 16) not in (0, stat.S_IFREG):
                raise ValueError('Pinned artifact members must be regular files')
        if apk_info.file_size != pin['apk_bytes'] or apk_info.file_size > 600 * 1024 * 1024:
            raise ValueError('Retained APK exceeds its pinned size')
        if not 0 < receipt_info.file_size <= 256 * 1024:
            raise ValueError('Build receipt exceeds the bounded size')
        receipt_bytes = outer.read(receipt_info)
        if hashlib.sha256(receipt_bytes).hexdigest() != pin['build_receipt_sha256']:
            raise ValueError('Build receipt differs from its immutable hash')
        receipt = json.loads(receipt_bytes)
        if (receipt['sorter_commit'] != pin['application_source']
                or receipt['sorter_tree'] != pin['application_tree']
                or receipt['platform'] != 'android' or receipt['package'] != pin['package']
                or receipt['application_source_clean'] is not True
                or receipt['artifact']['kind'] != 'apk'
                or receipt['artifact']['abis'] != [pin['abi']]
                or receipt['artifact']['sha256'] != pin['apk_sha256']
                or receipt['artifact']['bytes'] != pin['apk_bytes']):
            raise ValueError('Build receipt does not identify the pinned application bytes')
        (output / 'build-receipt.json').write_bytes(receipt_bytes)
        size = 0
        value = hashlib.sha256()
        with outer.open(apk_info) as source, (output / 'CrispSorter-ocr.apk').open('xb') as destination:
            for block in iter(lambda: source.read(1024 * 1024), b''):
                size += len(block)
                if size > pin['apk_bytes']:
                    raise ValueError('Actual APK bytes exceed the pinned size')
                value.update(block)
                destination.write(block)
        if size != pin['apk_bytes'] or value.hexdigest() != pin['apk_sha256']:
            raise ValueError('Staged APK size or SHA-256 differs from the immutable pin')
    result = {'schema_version': 1, 'kind': 'android-startup-probe-input', 'pin': pin,
              'application_rebuilt': False, 'staged_apk_sha256': value.hexdigest(),
              'scope': 'Exact retained application bytes; successful startup is not established by staging.'}
    (output / 'input-receipt.json').write_text(json.dumps(result, indent=2) + '\n')
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--archive', type=Path, required=True)
    parser.add_argument('--metadata', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--pin', type=Path, default=Path(__file__).with_name('artifact.json'))
    args = parser.parse_args()
    pin = json.loads(args.pin.read_text())
    result = stage(args.archive, json.loads(args.metadata.read_text()), args.output, pin)
    print(json.dumps({'status': 'staged', 'apk_sha256': result['staged_apk_sha256']}))


if __name__ == '__main__':
    main()
