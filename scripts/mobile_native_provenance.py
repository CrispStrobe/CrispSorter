#!/usr/bin/env python3
"""Retain source-derived native notices and verify their actual app packaging.

The symbol proof describes the unstripped runtime produced by the native build.
It does not claim that internal symbol names survive final application stripping
or that a vendored text-layout algorithm has passed reader compatibility tests.
"""
from __future__ import annotations

import hashlib
from pathlib import Path
import re
import subprocess
import zipfile

MAX_NOTICE_BYTES = 1024 * 1024
MAX_NATIVE_BYTES = 512 * 1024 * 1024
BASE_NOTICES = {
    'crispembed-LICENSE.txt': ('crispembed', 'LICENSE'),
    'crispembed-NOTICE.teleocr.txt': ('crispembed', 'NOTICE.teleocr'),
    'crispembed-NOTICE.marker.txt': ('crispembed', 'NOTICE.marker'),
    'crispembed-oxr-LICENSE.txt': ('crispembed', 'licenses/oxr-LICENSE'),
    'crispembed-oxr-NOTICE.txt': ('crispembed', 'licenses/oxr-NOTICE'),
    'crispembed-Apache-2.0.txt': ('crispembed', 'models/licenses/apache-2.0/LICENSE'),
    'ggml-LICENSE.txt': ('ggml', 'LICENSE'),
}
SHEEN_NOTICES = {
    'SheenBidi-LICENSE.txt': ('crispembed', 'vendor/sheenbidi/LICENSE'),
    'NOTICE.sheenbidi': ('crispembed', 'NOTICE.sheenbidi'),
    'SheenBidi-UPSTREAM.md': ('crispembed', 'vendor/sheenbidi/UPSTREAM.md'),
}
# These exact C entry points are referenced by the reviewed PDF renderer.
SHEEN_SYMBOLS = ('SBAlgorithmCreate', 'SBAlgorithmCreateParagraph',
                 'SBCodepointGetBidiType', 'SBCodepointGetGeneralCategory',
                 'SBLineGetRunCount', 'SBLineGetRunsPtr', 'SBParagraphCreateLine')


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def file_sha256(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def git(source, *args):
    return subprocess.run(['git', '-C', str(source), *args], check=True,
                          capture_output=True, text=True, timeout=30).stdout.strip()


def bounded_file(path, root):
    if path.is_symlink() or not path.is_file() or path.resolve() != path or not path.is_relative_to(root):
        raise ValueError('Native provenance file must be regular and source-contained: ' + str(path))
    if not 0 < path.stat().st_size <= MAX_NOTICE_BYTES:
        raise ValueError('Native provenance file exceeds its bounded size: ' + str(path))
    return path.read_bytes()


def checked_components(components, lock):
    required = lock.get('native_components', {})
    if not isinstance(required, dict) or set(required) - {'sheenbidi'}:
        raise ValueError('Runtime lock names an unsupported native component')
    if (not isinstance(components, dict) or set(components) - {'sheenbidi'}
            or any(not isinstance(value, dict) for value in components.values())):
        raise ValueError('Native provenance names an unsupported component')
    for name, pin in required.items():
        if (not isinstance(pin, dict) or set(pin) != {'source_tree'}
                or not re.fullmatch('[0-9a-f]{40}', str(pin['source_tree']))
                or components.get(name, {}).get('source_tree') != pin['source_tree']):
            raise ValueError('Native component source tree differs from its application lock: ' + name)


def defined_sheen_symbols(output):
    """Ignore undefined/imported names and archive member headings from nm."""
    found = set()
    for line in output.splitlines():
        match = re.fullmatch(r'\s*[0-9a-fA-F]+\s+[Tt]\s+_?(SB[A-Za-z0-9_]+)\s*', line)
        if match:
            found.add(match[1])
    missing = set(SHEEN_SYMBOLS) - found
    if missing:
        raise ValueError('Native artifact does not define its required SheenBidi implementation: ' + ', '.join(sorted(missing)))
    return list(SHEEN_SYMBOLS)


def collect(source, output, target, artifacts, lock, ndk=None):
    """Call after the clean source-pinned native build, before writing its receipt."""
    source, output = source.resolve(), output.resolve()
    components, notices = {}, dict(BASE_NOTICES)
    vendor = source / 'vendor/sheenbidi'
    if vendor.exists() or vendor.is_symlink() or (source / 'NOTICE.sheenbidi').exists():
        if vendor.is_symlink() or not vendor.is_dir() or vendor.resolve() != vendor:
            raise ValueError('SheenBidi source must be a regular vendored directory')
        source_tree = git(source, 'rev-parse', 'HEAD:vendor/sheenbidi')
        if not re.fullmatch('[0-9a-f]{40}', source_tree):
            raise ValueError('SheenBidi source has no immutable Git tree')
        upstream = bounded_file(vendor / 'UPSTREAM.md', source).decode('utf-8')
        revisions = re.findall(r'commit `([0-9a-f]{40})`', upstream)
        if len(revisions) != 1:
            raise ValueError('SheenBidi upstream provenance must name one immutable commit')
        symbols = {}
        for artifact in artifacts:
            if target.startswith('android-'):
                if ndk is None:
                    raise ValueError('The Android NDK is required for native symbol verification')
                nm = ndk / 'toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-nm'
                command = [str(nm), '--defined-only', '--extern-only', str(artifact)]
            else:
                command = ['xcrun', 'nm', '-gU', str(artifact)]
            result = subprocess.run(command, check=True, capture_output=True, text=True, timeout=120)
            if len(result.stdout) > 64 * 1024 * 1024:
                raise ValueError('Native symbol evidence exceeds its bounded size')
            symbols[artifact.name] = {'sha256': file_sha256(artifact),
                                      'defined_symbols': defined_sheen_symbols(result.stdout)}
        components['sheenbidi'] = {'source_path': 'vendor/sheenbidi', 'source_tree': source_tree,
                                   'upstream_commit': revisions[0], 'artifacts': symbols,
                                   'linkage': 'Unity C implementation in the primary native library'}
        notices.update(SHEEN_NOTICES)
    checked_components(components, lock)
    directory = output / 'notices'
    directory.mkdir()
    records = {}
    for basename, (repository, relative) in notices.items():
        checkout = source if repository == 'crispembed' else source / 'ggml'
        path = checkout / relative
        data = bounded_file(path, checkout)
        blob = hashlib.sha1(b'blob ' + str(len(data)).encode('ascii') + b'\0' + data).hexdigest()
        if git(checkout, 'rev-parse', 'HEAD:' + relative) != blob:
            raise ValueError('Native notice differs from its committed source: ' + relative)
        (directory / basename).write_bytes(data)
        records[basename] = {'source_repository': repository, 'source_path': relative,
                             'git_blob': blob, 'bytes': len(data), 'sha256': sha256(data)}
    return {'schema_version': 1, 'components': components, 'notices': records}


def verify_packaged(artifact, platform, runtime_directory, runtime, lock):
    """Bind native source notices to the bytes in the APK or actual .app bundle."""
    proof = runtime.get('native_provenance')
    if not isinstance(proof, dict) or proof.get('schema_version') != 1:
        raise ValueError('Native runtime is missing its source notice and component proof')
    components, notices = proof.get('components'), proof.get('notices')
    checked_components(components, lock)
    runtime_directory = runtime_directory.resolve()
    name = 'libcrispembed.so' if platform == 'android' else 'libcrispembed.a'
    if set(runtime.get('artifacts', {})) != {name}:
        raise ValueError('Native runtime artifact inventory differs from its app platform')
    library = runtime_directory / 'lib' / name
    if (library.is_symlink() or not library.is_file() or library.resolve() != library
            or not 0 < library.stat().st_size <= MAX_NATIVE_BYTES
            or file_sha256(library) != runtime['artifacts'][name]):
        raise ValueError('Built native artifact changed after its symbol proof')
    required = dict(BASE_NOTICES)
    if 'sheenbidi' in components:
        required.update(SHEEN_NOTICES)
        component = components['sheenbidi']
        if (component.get('source_path') != 'vendor/sheenbidi'
                or not re.fullmatch('[0-9a-f]{40}', str(component.get('source_tree')))
                or not re.fullmatch('[0-9a-f]{40}', str(component.get('upstream_commit')))
                or set(component.get('artifacts', {})) != set(runtime['artifacts'])):
            raise ValueError('Native component source/artifact identity is incomplete')
        for name, entry in component['artifacts'].items():
            if (entry.get('sha256') != runtime['artifacts'][name]
                    or entry.get('defined_symbols') != list(SHEEN_SYMBOLS)):
                raise ValueError('Native component symbol proof does not match its runtime artifact')
    if not isinstance(notices, dict) or set(notices) != set(required):
        raise ValueError('Native notice inventory is incomplete or unexpected')
    for basename, (repository, relative) in required.items():
        entry = notices[basename]
        if (entry.get('source_repository') != repository or entry.get('source_path') != relative
                or not re.fullmatch('[0-9a-f]{40}', str(entry.get('git_blob')))):
            raise ValueError('Native notice source identity is invalid: ' + basename)
        data = bounded_file(runtime_directory / 'notices' / basename, runtime_directory)
        blob = hashlib.sha1(b'blob ' + str(len(data)).encode('ascii') + b'\0' + data).hexdigest()
        if (len(data) != entry.get('bytes') or sha256(data) != entry.get('sha256')
                or blob != entry['git_blob']):
            raise ValueError('Native notice changed after its source build: ' + basename)
    packaged = {}
    if platform == 'android':
        with zipfile.ZipFile(artifact) as archive:
            names = archive.namelist()
            if len(names) != len(set(names)):
                raise ValueError('APK contains duplicate members')
            for basename, entry in notices.items():
                matches = [name for name in names if name.startswith('assets/') and Path(name).name == basename]
                if len(matches) != 1 or archive.getinfo(matches[0]).file_size != entry['bytes']:
                    raise ValueError('APK does not contain exactly one native notice: ' + basename)
                if sha256(archive.read(matches[0])) != entry['sha256']:
                    raise ValueError('Packaged native notice differs from its source: ' + basename)
                packaged[basename] = {'path': matches[0], 'sha256': entry['sha256'], 'bytes': entry['bytes']}
    else:
        artifact = artifact.resolve()
        for basename, entry in notices.items():
            matches = list(artifact.rglob(basename))
            if len(matches) != 1:
                raise ValueError('App does not contain exactly one native notice: ' + basename)
            data = bounded_file(matches[0], artifact)
            if len(data) != entry['bytes'] or sha256(data) != entry['sha256']:
                raise ValueError('Packaged native notice differs from its source: ' + basename)
            packaged[basename] = {'path': matches[0].relative_to(artifact).as_posix(),
                                  'sha256': entry['sha256'], 'bytes': entry['bytes']}
    return {'components': components, 'packaged_notices': packaged,
            'interpretation': 'Required native symbols were defined in the source-built runtime; notice bytes match the actual app package. This does not establish PDF reader compatibility.'}
