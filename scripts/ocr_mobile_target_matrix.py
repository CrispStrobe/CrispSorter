#!/usr/bin/env python3
"""Select the mobile qualification targets for push or manual runs."""
import json
import sys

TARGETS = [
    dict(runner='ubuntu-24.04', runtime='android-arm64', rust='aarch64-linux-android', cli='aarch64', abi='arm64-v8a', probe=False),
    dict(runner='ubuntu-24.04', runtime='android-x86_64', rust='x86_64-linux-android', cli='x86_64', abi='x86_64', probe=True),
    dict(runner='macos-15', runtime='ios-sim-arm64', rust='aarch64-apple-ios-sim', cli='aarch64-sim', abi='arm64', probe=True),
]

def matrix(target):
    selected = TARGETS if target == 'all' else [item for item in TARGETS if item['runtime'] == target]
    if not selected:
        raise ValueError(f'Unknown mobile target: {target}')
    return {'include': selected}

if __name__ == '__main__':
    print(json.dumps(matrix(sys.argv[1]), separators=(',', ':')))
