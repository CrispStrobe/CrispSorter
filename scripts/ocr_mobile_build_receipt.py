#!/usr/bin/env python3
"""Bind a mobile artifact to immutable application source and paired native code.

Run after building, before installation. This records identity; it does not
qualify OCR accuracy or device behavior. Existing receipts are never replaced.
The generated license inventory is recorded separately from application sources;
regenerating that data must not be misreported as an entirely clean Git tree.
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
import tempfile
import tomllib
import zipfile

from mobile_native_provenance import verify_packaged as verify_native_provenance

PACKAGE = 'com.crispstrobe.crispsorter'
MAX_LIBRARY_BYTES = 256 * 1024 * 1024
# The ARM64 debug build produced a 317,517,760-byte application .so.
# Keep paired third-party inputs at the smaller bound; inspect app members
# without loading their complete payloads into memory.
MAX_PACKAGED_LIBRARY_BYTES = 512 * 1024 * 1024
MAX_APK_UNCOMPRESSED_BYTES = 2 * 1024 * 1024 * 1024
MAX_APK_MEMBERS = 20_000
MAX_ELF_METADATA_BYTES = 8 * 1024 * 1024
INSPECTION_CHUNK_BYTES = 1024 * 1024


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


class ElfReader:
    """Random access to bounded metadata and streamed mapped-section hashes."""
    def __init__(self, source):
        self.source = source
        self.buffer = isinstance(source, (bytes, bytearray, memoryview))
        self.size = len(source) if self.buffer else os.fstat(source.fileno()).st_size

    def check_range(self, offset, count):
        if offset < 0 or count < 0 or offset + count > self.size:
            raise ValueError('ELF range exceeds the actual file bytes')

    def read(self, offset, count):
        self.check_range(offset, count)
        if count > MAX_ELF_METADATA_BYTES:
            raise ValueError('ELF metadata exceeds the inspection bound')
        if self.buffer:
            return bytes(memoryview(self.source)[offset:offset + count])
        self.source.seek(offset)
        value = self.source.read(count)
        if len(value) != count:
            raise ValueError('ELF metadata ended before its declared range')
        return value

    def hash(self, offset, count):
        self.check_range(offset, count)
        result = hashlib.sha256()
        if not self.buffer:
            self.source.seek(offset)
        for position in range(0, count, INSPECTION_CHUNK_BYTES):
            length = min(INSPECTION_CHUNK_BYTES, count - position)
            chunk = (memoryview(self.source)[offset + position:offset + position + length]
                     if self.buffer else self.source.read(length))
            if len(chunk) != length:
                raise ValueError('ELF section ended before its declared range')
            result.update(chunk)
        return result.hexdigest()


def elf_allocated_identity(data):
    """Compare mapped ELF content while permitting removal of debug sections.

    Gradle may strip symbols from a copied shared library. Comparing the whole
    file then rejects the same runtime, while checking only its filename accepts
    a stale runtime. Allocated sections, addresses and memory sizes identify the
    actual code/data independently of nonallocated debug and section-name data.
    Only the two supported little-endian Android ELF64 targets are admitted.
    """
    source = ElfReader(data)
    if source.size < 64 or source.read(0, 6) != b'\x7fELF\x02\x01':
        raise ValueError('Expected a little-endian ELF64 Android runtime')
    header = struct.unpack('<16sHHIQQQIHHHHHH', source.read(0, 64))
    _, kind, machine, _, entry, _, section_offset, flags, _, _, _, section_size, count, names_index = header
    if kind != 3 or machine not in (62, 183):
        raise ValueError('Runtime must be an Android x86_64 or arm64 shared library')
    if section_size != 64 or not 0 < count < 65536 or not 0 < names_index < count:
        raise ValueError('Unsupported ELF section table')
    if section_offset + section_size * count > source.size:
        raise ValueError('Truncated ELF section table')
    table = source.read(section_offset, section_size * count)
    sections = [struct.unpack_from('<IIQQQQIIQQ', table, i * section_size) for i in range(count)]
    names_section = sections[names_index]
    names_start, names_size = names_section[4:6]
    if names_start + names_size > source.size:
        raise ValueError('Truncated ELF section names')
    names = source.read(names_start, names_size)
    spans = sorted((section[4], section[4] + section[5]) for section in sections
                   if section[2] & 2 and section[1] != 8 and section[5])
    if any(left[1] > right[0] for left, right in zip(spans, spans[1:])):
        raise ValueError('Allocated ELF file sections overlap')
    allocated = []
    for name_at, section_type, section_flags, address, offset, size, _, _, alignment, _ in sections:
        if not section_flags & 2:  # SHF_ALLOC, excluding debug/symbol-only metadata.
            continue
        end = names.find(b'\0', name_at)
        if name_at >= len(names) or end < 0:
            raise ValueError('Invalid allocated ELF section name')
        name = names[name_at:end].decode('ascii')
        if section_type != 8 and offset + size > source.size:  # SHT_NOBITS has no file content.
            raise ValueError('Truncated allocated ELF section')
        allocated.append({'name': name, 'type': section_type, 'flags': section_flags,
                          'address': address, 'size': size, 'alignment': alignment,
                          'sha256': None if section_type == 8 else source.hash(offset, size)})
    if not allocated or not any(item['name'] == '.text' and item['size'] for item in allocated):
        raise ValueError('Runtime has no allocated executable text')
    return {'machine': machine, 'entry': entry, 'flags': flags, 'sections': allocated}


def android_load_identity(data, abi):
    """Inspect the final packaged ELF, independently of compiler/zip flags."""
    source = ElfReader(data)
    if source.size < 64 or source.read(0, 6) != b'\x7fELF\x02\x01':
        raise ValueError('Packaged Android library must be little-endian ELF64')
    header = struct.unpack('<16sHHIQQQIHHHHHH', source.read(0, 64))
    if header[1] != 3 or header[2] != {'arm64-v8a': 183, 'x86_64': 62}.get(abi):
        raise ValueError('Packaged Android library has the wrong shared-library target')
    offset, entry_size, count = header[5], header[9], header[10]
    if offset < 64 or entry_size != 56 or not 0 < count <= 128 or offset + count * entry_size > source.size:
        raise ValueError('Packaged Android library has invalid program headers')
    table = source.read(offset, entry_size * count)
    loads = []
    for index in range(count):
        kind, flags, start, address, _, size, memory_size, alignment = struct.unpack_from('<IIQQQQQQ', table, index * entry_size)
        if kind != 1:  # PT_LOAD; section alignment alone does not establish loadability.
            continue
        if (start + size > source.size or memory_size < size or address + memory_size >= 2**64
                or alignment < 16384 or alignment & (alignment - 1)
                or start % alignment != address % alignment):
            raise ValueError('Packaged Android library has invalid or non-16-KiB load segments')
        loads.append({'offset': start, 'address': address, 'file_bytes': size,
                      'memory_bytes': memory_size, 'flags': flags, 'alignment': alignment})
    if not loads or not any(item['flags'] & 1 and item['file_bytes'] for item in loads):
        raise ValueError('Packaged Android library has no executable load segment')
    return {'abi': abi, 'minimum_load_alignment': min(item['alignment'] for item in loads), 'load_segments': loads}


def apk_inventory(archive):
    members = archive.infolist()
    names = [entry.filename for entry in members]
    if len(members) > MAX_APK_MEMBERS or len(set(names)) != len(names):
        raise ValueError('APK member inventory exceeds its count bound or contains duplicates')
    if any(entry.file_size < 0 for entry in members) or sum(entry.file_size for entry in members) > MAX_APK_UNCOMPRESSED_BYTES:
        raise ValueError('APK exceeds the aggregate uncompressed inspection budget')
    return names


def inspect_apk_library(archive, name, abi, expected_allocated=None, limit=MAX_PACKAGED_LIBRARY_BYTES):
    """Stream one exact ZIP member to private scratch, then read only ELF metadata.

    Mapped-section hashes also stream. No full application .so byte buffer is
    created, and actual bytes must equal the independently bounded ZIP metadata.
    """
    entry = archive.getinfo(name)
    if not 0 < entry.file_size <= limit:
        raise ValueError('APK native library exceeds its member inspection budget: ' + name)
    sha, total = hashlib.sha256(), 0
    with tempfile.TemporaryFile() as staged:
        with archive.open(entry) as stream:
            while chunk := stream.read(INSPECTION_CHUNK_BYTES):
                total += len(chunk)
                if total > entry.file_size or total > limit:
                    raise ValueError('APK member actual bytes exceed its declared inspection budget')
                sha.update(chunk)
                staged.write(chunk)
        if total != entry.file_size:
            raise ValueError('APK member actual bytes differ from its ZIP metadata')
        staged.flush()
        elf = android_load_identity(staged, abi)
        if expected_allocated is not None and elf_allocated_identity(staged) != expected_allocated:
            raise ValueError('Packaged runtime code/data differs from the paired source build')
    return {'sha256': sha.hexdigest(), 'bytes': total, 'elf': elf}


def android_identity(artifact, runtime_directory, runtime):
    expected_abi = {'android-arm64': 'arm64-v8a', 'android-x86_64': 'x86_64'}.get(runtime['target'])
    if not expected_abi:
        raise ValueError('Native receipt is not an Android runtime')
    library = runtime_directory / 'lib' / 'libcrispembed.so'
    expected_hash = runtime['artifacts'].get('libcrispembed.so')
    if not library.is_file() or library.stat().st_size > MAX_LIBRARY_BYTES or digest(library) != expected_hash:
        raise ValueError('Built runtime bytes do not match their receipt')
    with library.open('rb') as stream:
        native_identity = elf_allocated_identity(stream)
    with zipfile.ZipFile(artifact) as archive:
        names = apk_inventory(archive)
        if archive.testzip() is not None:
            raise ValueError('APK contains duplicate members or failed archive integrity')
        libraries = [name for name in names if re.fullmatch(r'lib/[^/]+/[^/]+\.so', name)]
        abis = sorted({name.split('/')[1] for name in libraries})
        if abis != [expected_abi]:
            raise ValueError('APK native ABI does not match the paired runtime')
        packaged = f'lib/{expected_abi}/libcrispembed.so'
        if packaged not in names or archive.getinfo(packaged).file_size > MAX_LIBRARY_BYTES:
            raise ValueError('APK is missing its bounded CrispEmbed runtime')
        hashes, native_elf, sizes = {}, {}, {}
        for name in libraries:
            inspected = inspect_apk_library(archive, name, expected_abi,
                                            native_identity if name == packaged else None,
                                            MAX_LIBRARY_BYTES if name == packaged else MAX_PACKAGED_LIBRARY_BYTES)
            hashes[name] = inspected['sha256']
            native_elf[name] = inspected['elf']
            sizes[name] = inspected['bytes']
    return {'kind': 'apk', 'sha256': digest(artifact), 'bytes': artifact.stat().st_size,
            'abis': abis, 'native_libraries': hashes, 'native_elf': native_elf,
            'native_library_bytes': sizes,
            'inspection_budget': {'member_bytes': MAX_PACKAGED_LIBRARY_BYTES,
                                  'paired_runtime_bytes': MAX_LIBRARY_BYTES,
                                  'aggregate_uncompressed_bytes': MAX_APK_UNCOMPRESSED_BYTES,
                                  'maximum_elf_metadata_read_bytes': MAX_ELF_METADATA_BYTES,
                                  'stream_chunk_bytes': INSPECTION_CHUNK_BYTES},
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


def source_tree_identity(source):
    def paths(*args):
        output = subprocess.run(['git', '-C', str(source), *args], check=True,
                                capture_output=True, timeout=30).stdout
        if output and not output.endswith(b'\0'):
            raise ValueError('Source status was not NUL-delimited')
        return [item.decode('utf-8') for item in output.split(b'\0') if item]

    modified = paths('diff', '--name-only', '-z', 'HEAD', '--')
    untracked = paths('ls-files', '--others', '--exclude-standard', '-z')
    generated_path = 'static/licenses.json'
    if untracked or set(modified) - {generated_path}:
        raise ValueError('Application source or dependency lock changed after checkout; commit source changes first')
    generated = []
    if modified:
        inventory = source / generated_path
        if inventory.is_symlink() or not inventory.is_file() or inventory.resolve() != inventory:
            raise ValueError('Generated license inventory must be a regular source-contained file')
        if not 0 < inventory.stat().st_size <= 8 * 1024 * 1024:
            raise ValueError('Generated license inventory exceeds its bounded size')
        data = inventory.read_bytes()
        value = json.loads(data)
        if (not isinstance(value, dict) or set(value) != {'generatedAt', 'counts', 'licenses'}
                or not isinstance(value['generatedAt'], str)):
            raise ValueError('Generated license inventory has an unknown schema')
        generated_at = datetime.fromisoformat(value['generatedAt'].replace('Z', '+00:00'))
        counts, licenses = value['counts'], value['licenses']
        if (generated_at.tzinfo is None or not isinstance(counts, dict)
                or set(counts) not in ({'frontend', 'backend', 'total'}, {'frontend', 'backend', 'native', 'total'})
                or any(type(count) is not int or count < 0 for count in counts.values())
                or not isinstance(licenses, list) or len(licenses) > 20_000
                or any(not isinstance(item, dict) or not isinstance(item.get('name'), str)
                       or item.get('source') not in ('Frontend', 'Backend', 'Native') for item in licenses)
                or counts != {**{'frontend': sum(item['source'] == 'Frontend' for item in licenses),
                                 'backend': sum(item['source'] == 'Backend' for item in licenses),
                                 'total': len(licenses)},
                              **({'native': sum(item['source'] == 'Native' for item in licenses)} if 'native' in counts else {})}
                or ('native' not in counts and any(item['source'] == 'Native' for item in licenses))):
            raise ValueError('Generated license inventory counts/content are inconsistent')
        committed = subprocess.run(['git', '-C', str(source), 'show', 'HEAD:' + generated_path],
                                   check=True, capture_output=True, timeout=30).stdout
        generated.append({'path': generated_path, 'bytes': len(data), 'sha256': hashlib.sha256(data).hexdigest(),
                          'committed_sha256': hashlib.sha256(committed).hexdigest(),
                          'generated_at': value['generatedAt'], 'counts': counts})
    return {'application_source_clean': True, 'tracked_source_clean': not modified,
            'generated_tracked_files': generated, 'cargo_lock_sha256': digest(source / 'Cargo.lock')}


def ort_identity(source, platform, artifact, runtime_path, expected_target):
    """Bind the extra indexing runtime to pinned inputs and packaged bytes."""
    lock_path = source / 'scripts/ocr-mobile-ort-lock.json'
    lock = json.loads(lock_path.read_text())
    runtime = json.loads(runtime_path.read_text())
    directory = runtime_path.parent.resolve()
    if runtime.get('kind') != 'ocr-mobile-ort-runtime' or runtime.get('target') != expected_target:
        raise ValueError('ONNX Runtime receipt target differs from the app target')
    if runtime.get('lock_sha256') != digest(lock_path) or runtime.get('rust_binding') != lock['rust_binding'] or runtime.get('api_version') != lock['api_version']:
        raise ValueError('ONNX Runtime receipt differs from the current binding/archive lock')
    manifest = tomllib.loads((source / 'src-tauri/Cargo.toml').read_text())
    if manifest['dependencies']['ort'] != '=' + lock['rust_binding']:
        raise ValueError('Application ort binding changed; review the mobile native ABI pin')
    pin = lock['android' if platform == 'android' else 'ios']
    if runtime.get('archive') != {key: pin[key] for key in ('url', 'filename', 'bytes', 'sha256')} or runtime.get('version') != pin['version']:
        raise ValueError('ONNX Runtime package identity differs from the committed pin')
    env = tomllib.loads((source / '.cargo/config.toml').read_text()).get('env', {})
    required = ({'ORT_LIB_LOCATION': str(directory / 'lib'), 'ORT_PREFER_DYNAMIC_LINK': '1'} if platform == 'android'
                else {'ORT_IOS_XCFWK_LOCATION': str(directory / 'onnxruntime.xcframework')})
    for name, value in required.items():
        if env.get(name) != {'value': value, 'force': True}:
            raise ValueError('Nested Cargo did not pin the qualified ONNX Runtime: ' + name)
    for name, entry in runtime['files'].items():
        item = directory / name
        if item.is_symlink() or not item.is_file() or item.resolve() != item or not item.is_relative_to(directory):
            raise ValueError('ONNX Runtime receipt names a missing or unsafe file')
        if item.stat().st_size != entry['bytes'] or digest(item) != entry['sha256']:
            raise ValueError('Qualified ONNX Runtime changed after extraction')
    header = directory / 'include/onnxruntime_c_api.h'
    if not header.is_file() or digest(header) != lock['header_sha256']:
        raise ValueError('Qualified ONNX Runtime header differs from the API pin')
    notices = {'onnxruntime-LICENSE.txt': lock['notices'][0], 'onnxruntime-ThirdPartyNotices.txt': lock['notices'][1]}
    packaged_notices = {}
    if platform == 'android':
        abi = {'android-arm64': 'arm64-v8a', 'android-x86_64': 'x86_64'}[expected_target]
        native = directory / 'lib/libonnxruntime.so'
        if digest(native) != pin['libraries'][abi]['sha256']:
            raise ValueError('ONNX Runtime library differs from its immutable binary pin')
        with zipfile.ZipFile(artifact) as archive:
            name = f'lib/{abi}/libonnxruntime.so'
            if name not in apk_inventory(archive) or archive.getinfo(name).file_size > MAX_LIBRARY_BYTES:
                raise ValueError('APK is missing its bounded ONNX Runtime library')
            with native.open('rb') as stream:
                expected = elf_allocated_identity(stream)
            inspected = inspect_apk_library(archive, name, abi, expected, MAX_LIBRARY_BYTES)
            for basename, notice in notices.items():
                candidates = [item for item in archive.namelist() if Path(item).name == basename]
                if len(candidates) != 1 or archive.getinfo(candidates[0]).file_size != notice['bytes']:
                    raise ValueError('APK does not contain the ONNX Runtime notice: ' + basename)
                if hashlib.sha256(archive.read(candidates[0])).hexdigest() != notice['sha256']:
                    raise ValueError('Packaged ONNX Runtime notice changed')
                packaged_notices[basename] = {'path': candidates[0], 'sha256': notice['sha256']}
        linkage = {'packaged_library': name, 'packaged_sha256': inspected['sha256'], 'mapped_sections_match': True}
    else:
        identifier = 'ios-arm64_x86_64-simulator' if expected_target == 'ios-sim-arm64' else 'ios-arm64'
        binary = directory / 'onnxruntime.xcframework' / identifier / 'onnxruntime.framework/onnxruntime'
        if digest(binary) != pin['libraries'][identifier]['sha256']:
            raise ValueError('iOS ONNX Runtime framework differs from its immutable binary pin')
        info = plistlib.loads((artifact / 'Info.plist').read_bytes())
        executable = info.get('CFBundleExecutable', '')
        if not executable or Path(executable).name != executable:
            raise ValueError('iOS application executable identity is invalid')
        symbols = subprocess.run(['xcrun', 'nm', '-m', str(artifact / executable)], capture_output=True, text=True, check=True, timeout=60).stdout
        if not any('_OrtGetApiBase' in line and '(undefined)' not in line for line in symbols.splitlines()):
            raise ValueError('Built iOS app does not define the ONNX Runtime C API symbol')
        for basename, notice in notices.items():
            candidates = [item for item in artifact.rglob(basename) if item.is_file() and not item.is_symlink()]
            if len(candidates) != 1 or candidates[0].stat().st_size != notice['bytes'] or digest(candidates[0]) != notice['sha256']:
                raise ValueError('App does not contain the qualified ONNX Runtime notice: ' + basename)
            packaged_notices[basename] = {'path': candidates[0].relative_to(artifact).as_posix(), 'sha256': notice['sha256']}
        linkage = {'linked_c_api_symbol': 'OrtGetApiBase', 'qualified_static_framework_sha256': digest(binary),
                   'interpretation': 'Qualified archive/config and defined app symbol; static linking transforms the archive, so this is not byte equality with the executable.'}
    return {'receipt_sha256': digest(runtime_path), 'runtime': runtime, 'linkage': linkage, 'packaged_notices': packaged_notices}


def record(source, platform, artifact, runtime_path, ort_runtime_path):
    source, artifact, runtime_path = source.resolve(), artifact.resolve(), runtime_path.resolve()
    source_identity = source_tree_identity(source)
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
    ort = ort_identity(source, platform, artifact, ort_runtime_path.resolve(), runtime['target'])
    native_provenance = verify_native_provenance(artifact, platform, runtime_path.parent, runtime, lock)
    return {'schema_version': 2, 'kind': 'ocr-mobile-app-build',
            'recorded_at': datetime.now(timezone.utc).isoformat(),
            'platform': platform, 'package': PACKAGE,
            'sorter_commit': source_commit, 'sorter_tree': tree, **source_identity,
            'crispembed_commit': runtime['source_commit'], 'ggml_commit': runtime['ggml_commit'],
            'runtime_receipt_sha256': digest(runtime_path), 'runtime': runtime,
            'cargo_config_sha256': digest(config) if config.is_file() else None,
            'onnx_runtime': ort,
            'native_provenance': native_provenance,
            'github': {key: os.environ.get('GITHUB_' + key.upper()) for key in ('run_id', 'run_attempt', 'job', 'sha', 'ref')},
            'artifact': identity, 'qualification': 'Build identity only; application behavior and OCR quality require executed tests.'}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source', type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument('--platform', required=True, choices=('android', 'ios-simulator', 'ios-device'))
    parser.add_argument('--artifact', required=True, type=Path)
    parser.add_argument('--runtime-receipt', required=True, type=Path)
    parser.add_argument('--ort-runtime-receipt', required=True, type=Path)
    parser.add_argument('--output', required=True, type=Path)
    args = parser.parse_args()
    receipt = record(args.source, args.platform, args.artifact, args.runtime_receipt, args.ort_runtime_receipt)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with args.output.open('x') as stream:
        json.dump(receipt, stream, indent=2)
        stream.write('\n')
    print(json.dumps(receipt, indent=2))


if __name__ == '__main__':
    main()
