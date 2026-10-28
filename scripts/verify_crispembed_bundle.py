#!/usr/bin/env python3
"""Verify downloaded native runtime bytes against the reviewed immutable release."""
import argparse
import hashlib
import json
import subprocess
from pathlib import Path


def verify(path: Path, asset: str, lock_path: Path | None = None) -> dict:
    lock = json.loads((lock_path or Path(__file__).with_name('crispembed-runtime-lock.json')).read_text())
    expected = lock['assets'].get(asset)
    if expected is None:
        raise ValueError(f'Asset is not qualified by the runtime lock: {asset}')
    digest = hashlib.sha256()
    size = 0
    with path.open('rb') as bundle:
        for chunk in iter(lambda: bundle.read(1024 * 1024), b''):
            digest.update(chunk)
            size += len(chunk)
    if size != expected['bytes'] or digest.hexdigest() != expected['sha256']:
        raise ValueError(f'Runtime identity mismatch for {asset}; refusing extraction')
    return {'asset': asset, 'version': lock['version'], 'source_commit': lock['source_commit'],
            'sha256': digest.hexdigest(), 'bytes': size}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('bundle', type=Path)
    parser.add_argument('--asset', required=True)
    parser.add_argument('--source', type=Path)
    args = parser.parse_args()
    report = verify(args.bundle, args.asset)
    if args.source:
        actual = subprocess.check_output(['git', '-C', str(args.source), 'rev-parse', 'HEAD'], text=True).strip()
        if actual != report['source_commit']:
            raise ValueError('CrispEmbed bindings do not match the verified native runtime source')
    args.bundle.with_suffix(args.bundle.suffix + '.verified.json').write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps(report))
