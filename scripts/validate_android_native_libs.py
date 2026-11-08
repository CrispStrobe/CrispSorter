#!/usr/bin/env python3
"""Check an Android native library and its non-system DT_NEEDED closure.

Libraries must be flat in one directory. The emitted filenames can be copied
into the APK's jniLibs/<abi> directory after validation succeeds.
"""
import argparse
from pathlib import Path
import re
import subprocess
import sys

MACHINES = {'arm64-v8a': 'AArch64', 'armeabi-v7a': 'ARM', 'x86_64': 'Advanced Micro Devices X86-64'}
# Public Android NDK system libraries; app-provided libraries (including
# libc++_shared.so) must be present and are validated recursively.
SYSTEM_LIBS = frozenset('libc.so libm.so libdl.so liblog.so libandroid.so libz.so libEGL.so libGLESv1_CM.so libGLESv2.so libGLESv3.so libOpenSLES.so libvulkan.so libjnigraphics.so libmediandk.so libnativewindow.so'.split())


def closure(directory: Path, abi: str, root: str = 'libcrispembed.so') -> list[str]:
    if abi not in MACHINES:
        raise ValueError(f'unsupported ABI: {abi}')
    pending, seen = [root], set()
    while pending:
        name = pending.pop()
        if name in seen or name in SYSTEM_LIBS:
            continue
        if Path(name).name != name:
            raise ValueError(f'invalid dependency name: {name}')
        path = directory / name
        if not path.is_file():
            raise ValueError(f'missing Android native dependency: {path}')
        try:
            result = subprocess.run(['readelf', '-h', '-d', str(path)], check=True, capture_output=True, text=True)
        except (OSError, subprocess.CalledProcessError) as exc:
            raise ValueError(f'cannot inspect Android native library {path}: {exc}') from exc
        machine = re.search(r'^\s*Machine:\s*(.+)$', result.stdout, re.MULTILINE)
        kind = re.search(r'^\s*Type:\s*(\S+)', result.stdout, re.MULTILINE)
        if not machine or machine.group(1).strip() != MACHINES[abi] or not kind or kind.group(1) != 'DYN':
            raise ValueError(f'{path} is not an Android {abi} shared ELF library')
        seen.add(name)
        pending.extend(re.findall(r'\(NEEDED\).*?\[(.*?)\]', result.stdout))
    return sorted(seen)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('directory', type=Path)
    parser.add_argument('--abi', required=True, choices=MACHINES)
    args = parser.parse_args()
    try:
        print('\n'.join(closure(args.directory, args.abi)))
    except ValueError as exc:
        print(str(exc), file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
