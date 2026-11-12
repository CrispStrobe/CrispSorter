#!/usr/bin/env python3
"""Resolve the selected committed runtime identity before Android app testing."""
import argparse
import json
from pathlib import Path

from ocr_mobile_build_receipt import digest, git, runtime_lock_identity


def verified_embed_commit(source, lock_path, build_path, runtime_path, expected_source=None):
    source = source.resolve()
    lock, identity = runtime_lock_identity(source, lock_path)
    build = json.loads(build_path.read_text())
    runtime = json.loads(runtime_path.read_text())
    if expected_source and expected_source != lock['source_commit']:
        raise ValueError('Selected runtime source differs from the workflow source')
    if build.get('runtime_lock') != identity:
        raise ValueError('Application build receipt does not match the selected committed runtime lock')
    if build.get('sorter_commit') != git(source, 'rev-parse', 'HEAD'):
        raise ValueError('Application build receipt does not match this application source')
    if (build.get('runtime_receipt_sha256') != digest(runtime_path)
            or build.get('runtime') != runtime):
        raise ValueError('Application and native runtime receipts differ')
    if (build.get('crispembed_commit') != lock['source_commit']
            or build.get('ggml_commit') != lock['ggml_commit']
            or any(runtime.get(key) != lock[key] for key in ('source_commit', 'ggml_commit'))):
        raise ValueError('Application or runtime source differs from the selected committed lock')
    if build.get('platform') != 'android' or runtime.get('target') != 'android-x86_64':
        raise ValueError('Android emulator qualification requires its x86_64 build receipt')
    return lock['source_commit']


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source', type=Path, required=True)
    parser.add_argument('--runtime-lock', default='scripts/ocr-mobile-runtime-lock.json')
    parser.add_argument('--expected-source')
    parser.add_argument('--build-receipt', type=Path, required=True)
    parser.add_argument('--runtime-receipt', type=Path, required=True)
    args = parser.parse_args()
    print(verified_embed_commit(args.source, args.runtime_lock, args.build_receipt,
                                args.runtime_receipt, args.expected_source))


if __name__ == '__main__':
    main()
