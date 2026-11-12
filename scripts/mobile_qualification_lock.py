#!/usr/bin/env python3
"""Select a repository-contained immutable lock for mobile qualification only."""
import json
from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]


def select_lock(path, root=ROOT):
    root = root.resolve()
    requested = Path(path)
    if requested.is_absolute() or '..' in requested.parts or any(c in str(path) for c in '\r\n'):
        raise ValueError('runtime lock must be a repository-relative path')
    resolved = (root / requested).resolve(strict=True)
    if not resolved.is_relative_to(root) or not resolved.is_file():
        raise ValueError('runtime lock must be a file inside the repository')
    relative = resolved.relative_to(root).as_posix()
    # Require the exact file bytes from this app commit. A dispatch cannot inject
    # an unreviewed lock or change the default release lock in the workspace.
    committed = subprocess.check_output(
        ['git', '-C', str(root), 'show', 'HEAD:' + relative])
    if resolved.read_bytes() != committed:
        raise ValueError('runtime lock differs from its committed bytes')
    lock = json.loads(committed)
    for key in ('source_commit', 'ggml_commit'):
        if not re.fullmatch('[0-9a-f]{40}', str(lock.get(key, ''))):
            raise ValueError(f'runtime lock requires a full immutable {key}')
    return {'source': lock['source_commit'], 'lock': relative}


if __name__ == '__main__':
    try:
        selected = select_lock(sys.argv[1])
        for key, value in selected.items():
            print(f'{key}={value}')
    except (ValueError, OSError, subprocess.CalledProcessError) as exc:
        sys.exit(f'invalid mobile qualification lock: {exc}')
