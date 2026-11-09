#!/usr/bin/env python3
"""Bind a mobile app artifact to its clean source tree and paired native build.

Run after building, before installation. This records identity; it does not
qualify OCR accuracy or device behavior. Existing receipts are never replaced.
"""
from __future__ import annotations

import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import plistlib
import re
import struct
import subprocess
import zipfile

PACKAGE = 'com.crispstrobe.crispsorter'
MAX_LIBRARY_BYTES = 256 * 1024 * 1024


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def elf_allocated_identity(data):
    """Compare mapped ELF content while permitting removal of debug sections.

    Gradle may strip symbols from a copied shared library. Comparing the whole
    file then rejects the same runtime, while checking only its filename accepts
    a stale runtime. Allocated sections, addresses and memory sizes identify the
    actual code/data independently of nonallocated debug and section-name data.
    Only the two supported little-endian Android ELF64 targets are admitted.
    """
    if len(data) < 64 or data[:6] != b'\x7fELF\x02\x01':
        raise ValueError('Expected a little-endian ELF64 Android runtime')
    header = struct.unpack_from('<16sHHIQQQIHHHHHH', data)
    _, kind, machine, _, entry, _, section_offset, flags, _, _, _, section_size, count, names_index = header
    if kind != 3 or machine not in (62, 183):
        raise ValueError('Runtime must be an Android x86_64 or arm64 shared library')
    if section_size != 64 or not 0 < count < 65536 or not 0 < names_index < count:
        raise ValueError('Unsupported ELF section table')
    if section_offset + section_size * count > len(data):
        raise ValueError('Truncated ELF section table')
    sections = [struct.unpack_from('<IIQQQQIIQQ', data, section_offset + i * section_size) for i in range(count)]
    names_section = sections[names_index]
    names_start, names_size = names_section[4:6]
    if names_start + names_size > len(data):
        raise ValueError('Truncated ELF section names')
    names = data[names_start:names_start + names_size]
    allocated = []
    for name_at, section_type, section_flags, address, offset, size, _, _, alignment, _ in sections:
        if not section_flags & 2:  # SHF_ALLOC, excluding debug/symbol-only metadata.
            continue
        end = names.find(b'\0', name_at)
        if name_at >= len(names) or end < 0:
            raise ValueError('Invalid allocated ELF section name')
        name = names[name_at:end].decode('ascii')
        if section_type != 8 and offset + size > len(data):  # SHT_NOBITS has no file content.
            raise ValueError('Truncated allocated ELF section')
        allocated.append({'name': name, 'type': section_type, 'flags': section_flags,
                          'address': address, 'size': size, 'alignment': alignment,
                          'sha256': None if section_type == 8 else hashlib.sha256(data[offset:offset + size]).hexdigest()})
    if not allocated or not any(item['name'] == '.text' and item['size'] for item in allocated):
        raise ValueError('Runtime has no allocated executable text')
    return {'machine': machine, 'entry': entry, 'flags': flags, 'sections': allocated}


def android_identity(artifact, runtime_directory, runtime):
    expected_abi = {'android-arm64': 'arm64-v8a', 'android-x86_64': 'x86_64'}.get(runtime['target'])
    if not expected_abi:
        raise ValueError('Native receipt is not an Android runtime')
    library = runtime_directory / 'lib' / 'libcrispembed.so'
    expected_hash = runtime['artifacts'].get('libcrispembed.so')
    if not library.is_file() or library.stat().st_size > MAX_LIBRARY_BYTES or digest(library) != expected_hash:
        raise ValueError('Built runtime bytes do not match their receipt')
    native_identity = elf_allocated_identity(library.read_bytes())
    with zipfile.ZipFile(artifact) as archive:
        names = archive.namelist()
        if len(set(names)) != len(names) or archive.testzip() is not None:
            raise ValueError('APK contains duplicate members or failed archive integrity')
        libraries = [name for name in names if re.fullmatch(r'lib/[^/]+/[^/]+\.so', name)]
        abis = sorted({name.split('/')[1] for name in libraries})
        if abis != [expected_abi]:
            raise ValueError('APK native ABI does not match the paired runtime')
        packaged = f'lib/{expected_abi}/libcrispembed.so'
        if packaged not in names or archive.getinfo(packaged).file_size > MAX_LIBRARY_BYTES:
            raise ValueError('APK is missing its bounded CrispEmbed runtime')
        packaged_data = archive.read(packaged)
        if elf_allocated_identity(packaged_data) != native_identity:
            raise ValueError('Packaged runtime code/data differs from the paired source build')
        hashes = {}
        for name in libraries:
            if archive.getinfo(name).file_size > MAX_LIBRARY_BYTES:
                raise ValueError('APK native library exceeds the evidence size bound')
            hashes[name] = hashlib.sha256(archive.read(name)).hexdigest()
    return {'kind': 'apk', 'sha256': digest(artifact), 'bytes': artifact.stat().st_size,
            'abis': abis, 'native_libraries': hashes,
            'paired_runtime_mapped_sections_match': True,
            'native_identity_sha256': hashlib.sha256(json.dumps(native_identity, sort_keys=True).encode()).hexdigest()}


def bundle_identity(artifact, expected_platform):
    if artifact.is_symlink() or not artifact.is_dir() or artifact.suffix != '.app':
        raise ValueError('iOS identity requires the actual .app directory')
    info = plistlib.loads((artifact / 'Info.plist').read_bytes())
    if info.get('CFBundleIdentifier') != PACKAGE or info.get('DTPlatformName') != expected_platform:
        raise ValueError('Unexpected app identifier or Apple platform')
    files = []
    for item in sorted(artifact.rglob('*')):
        if item.is_symlink():
            raise ValueError('App bundle symlinks require an explicit identity policy')
        if item.is_file():
            files.append({'path': item.relative_to(artifact).as_posix(), 'bytes': item.stat().st_size, 'sha256': digest(item)})
    return {'kind': 'app-bundle', 'bundle_identifier': PACKAGE, 'platform': expected_platform,
            'files': files, 'sha256': hashlib.sha256(json.dumps(files, sort_keys=True).encode()).hexdigest()}


def git(source, *args):
    return subprocess.run(['git', '-C', str(source), *args], check=True, text=True,
                          capture_output=True, timeout=30).stdout.strip()


def record(source, platform, artifact, runtime_path):
    source, artifact, runtime_path = source.resolve(), artifact.resolve(), runtime_path.resolve()
    modified = git(source, 'diff', '--name-only', 'HEAD', '--')
    untracked = git(source, 'ls-files', '--others', '--exclude-standard')
    if modified or untracked:
        raise ValueError('Record an app built from a clean tracked source tree; commit source changes first')
    source_commit = git(source, 'rev-parse', 'HEAD')
    tree = git(source, 'rev-parse', 'HEAD^{tree}')
    runtime = json.loads(runtime_path.read_text())
    lock = json.loads((source / 'scripts/ocr-mobile-runtime-lock.json').read_text())
    if runtime.get('source_commit') != lock['source_commit'] or runtime.get('ggml_commit') != lock['ggml_commit']:
        raise ValueError('Native runtime receipt does not match this application source lock')
    for key in ('source_commit', 'ggml_commit'):
        if not re.fullmatch('[0-9a-f]{40}', runtime[key]):
            raise ValueError('Native source identity requires full commit IDs')
    if platform == 'android':
        identity = android_identity(artifact, runtime_path.parent, runtime)
    else:
        expected_target = 'ios-sim-arm64' if platform == 'ios-simulator' else 'ios-arm64'
        if runtime['target'] != expected_target:
            raise ValueError('App platform does not match the native runtime target')
        identity = bundle_identity(artifact, 'iphonesimulator' if platform == 'ios-simulator' else 'iphoneos')
    config = source / '.cargo/config.toml'
    return {'schema_version': 1, 'kind': 'ocr-mobile-app-build',
            'recorded_at': datetime.now(timezone.utc).isoformat(),
            'platform': platform, 'package': PACKAGE,
            'sorter_commit': source_commit, 'sorter_tree': tree, 'tracked_source_clean': True,
            'crispembed_commit': runtime['source_commit'], 'ggml_commit': runtime['ggml_commit'],
            'runtime_receipt_sha256': digest(runtime_path), 'runtime': runtime,
            'cargo_config_sha256': digest(config) if config.is_file() else None,
            'github': {key: os.environ.get('GITHUB_' + key.upper()) for key in ('run_id', 'run_attempt', 'job', 'sha', 'ref')},
            'artifact': identity, 'qualification': 'Build identity only; application behavior and OCR quality require executed tests.'}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source', type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument('--platform', required=True, choices=('android', 'ios-simulator', 'ios-device'))
    parser.add_argument('--artifact', required=True, type=Path)
    parser.add_argument('--runtime-receipt', required=True, type=Path)
    parser.add_argument('--output', required=True, type=Path)
    args = parser.parse_args()
    receipt = record(args.source, args.platform, args.artifact, args.runtime_receipt)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with args.output.open('x') as stream:
        json.dump(receipt, stream, indent=2)
        stream.write('\n')
    print(json.dumps(receipt, indent=2))


if __name__ == '__main__':
    main()
