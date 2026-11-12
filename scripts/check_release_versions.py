#!/usr/bin/env python3
"""Check release-facing app versions before creating or changing draft assets."""
import argparse
import json
from pathlib import Path
import re
import sys
import tomllib


VERSION = re.compile(r'(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)')


def check_versions(root):
    root = Path(root)
    config = json.loads((root / 'src-tauri/tauri.conf.json').read_text())
    package = json.loads((root / 'package.json').read_text())
    npm_lock = json.loads((root / 'package-lock.json').read_text())
    cargo = tomllib.loads((root / 'src-tauri/Cargo.toml').read_text())['package']
    cargo_lock = tomllib.loads((root / 'Cargo.lock').read_text())
    version = config.get('version')
    if not isinstance(version, str) or not VERSION.fullmatch(version):
        raise ValueError('Tauri release version must be an exact numeric major.minor.patch')
    npm_root = npm_lock.get('packages', {}).get('', {})
    for label, value in [('package.json', package.get('version')),
                         ('package-lock.json', npm_lock.get('version')),
                         ('package-lock.json root package', npm_root.get('version')),
                         ('src-tauri/Cargo.toml', cargo.get('version'))]:
        if value != version:
            raise ValueError(f'{label} version does not match Tauri version {version}')
    if not package.get('name') or npm_lock.get('name') != package['name'] or npm_root.get('name') != package['name']:
        raise ValueError('npm lock identity does not match the frontend package')
    app = [item for item in cargo_lock.get('package', [])
           if item.get('name') == cargo.get('name') and 'source' not in item]
    if len(app) != 1 or app[0].get('version') != version:
        raise ValueError('Cargo.lock must contain exactly one matching local application version')
    # Workspace defaults and unrelated dependency versions are independent of
    # the app's explicit release version and must not be rewritten for a bump.
    return version


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, default=Path(__file__).resolve().parent.parent)
    args = parser.parse_args()
    try:
        version = check_versions(args.root)
    except (OSError, ValueError, KeyError, TypeError) as error:
        print(f'::error::{error}', file=sys.stderr)
        sys.exit(1)
    print(f'Release app versions match {version} (v{version}).')


if __name__ == '__main__':
    main()
