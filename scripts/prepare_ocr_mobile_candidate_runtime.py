#!/usr/bin/env python3
"""Stage only the reviewed x86_64 candidate archive; never rebuild on failure."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import stat
import zipfile

from build_mobile_crispembed import check_source
from mobile_native_provenance import BASE_NOTICES, SHEEN_NOTICES, checked_components
from mobile_qualification_lock import select_lock
from validate_android_native_libs import closure

ROOT = Path(__file__).resolve().parents[1]
PIN = 'scripts/ocr-mobile-candidate-artifact-lock.json'


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def committed_pin(root):
    import subprocess
    data = subprocess.check_output(['git', '-C', str(root), 'show', 'HEAD:' + PIN])
    if (root / PIN).read_bytes() != data:
        raise ValueError('Candidate artifact pin differs from its committed bytes')
    return json.loads(data)


def verify_staged(output, pin):
    for name, expected in pin['files'].items():
        relative = Path(name)
        if relative.parts[0] == 'crispembed-mobile':
            relative = Path(*relative.parts[1:])
        path = output / relative
        if (path.is_symlink() or not path.is_file() or path.resolve() != path
                or path.stat().st_size != expected['bytes'] or digest(path) != expected['sha256']):
            raise ValueError('Staged candidate differs from its reviewed artifact: ' + name)


def unpack(archive, output, pin):
    if digest(archive) != pin['archive_sha256']:
        raise ValueError('Candidate archive differs from its reviewed SHA-256 pin')
    if output.exists():
        raise ValueError('Candidate output must not exist')
    with zipfile.ZipFile(archive) as zipped:
        names = zipped.namelist()
        if len(names) != len(set(names)) or set(names) != set(pin['files']):
            raise ValueError('Candidate archive inventory differs from its reviewed pin')
        if sum(item.file_size for item in zipped.infolist()) > 512 * 1024 * 1024:
            raise ValueError('Candidate archive exceeds its total size limit')
        # Validate every member before writing. Only bounded, regular files at
        # the reviewed relative paths are accepted; extractall is never used.
        for name in names:
            member, expected = zipped.getinfo(name), pin['files'][name]
            path = Path(name)
            mode = member.external_attr >> 16
            if (path.is_absolute() or '..' in path.parts or member.is_dir()
                    or stat.S_IFMT(mode) not in (0, stat.S_IFREG) or member.file_size != expected['bytes']
                    or not 0 < member.file_size <= 512 * 1024 * 1024):
                raise ValueError('Invalid candidate archive member: ' + name)
            with zipped.open(member) as stream:
                if hashlib.file_digest(stream, 'sha256').hexdigest() != expected['sha256']:
                    raise ValueError('Candidate archive member hash mismatch: ' + name)
        output.mkdir(parents=True)
        for name in names:
            relative = Path(name)
            if relative.parts[0] == 'crispembed-mobile':
                relative = Path(*relative.parts[1:])
            destination = output / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            with zipped.open(name) as source, destination.open('xb') as target:
                shutil.copyfileobj(source, target)


def prepare(archive, output, source, root=ROOT):
    root, source, output = root.resolve(), source.resolve(), output.resolve()
    pin = committed_pin(root)
    selected = select_lock(pin['runtime_lock'], root)
    lock_path = root / selected['lock']
    lock = json.loads(lock_path.read_bytes())
    if output == source or source in output.parents or output in source.parents:
        raise ValueError('Candidate output must be outside the source checkout')
    check_source(source, lock)
    unpack(archive, output, pin)
    if (output / 'source-lock.json').read_bytes() != lock_path.read_bytes():
        raise ValueError('Archived source lock differs from the selected committed lock')
    runtime = json.loads((output / 'runtime-build.json').read_text())
    if (runtime['target'] != pin['target'] or pin['target'] != 'android-x86_64'
            or any(runtime[key] != lock[key] for key in ('source_commit', 'ggml_commit'))
            or runtime['artifacts'] != {'libcrispembed.so': digest(output / 'lib/libcrispembed.so')}):
        raise ValueError('Candidate runtime source, target or library does not match its receipt')
    proof = runtime['native_provenance']
    checked_components(proof['components'], lock)
    required = {**BASE_NOTICES, **SHEEN_NOTICES}
    if set(proof['notices']) != set(required):
        raise ValueError('Candidate source notice inventory is incomplete')
    for name, (repository, relative) in required.items():
        checkout = source if repository == 'crispembed' else source / 'ggml'
        if (output / 'notices' / name).read_bytes() != (checkout / relative).read_bytes():
            raise ValueError('Candidate notice differs from its checked source: ' + name)
    if closure(output / 'lib', 'x86_64') != ['libcrispembed.so']:
        raise ValueError('Candidate runtime does not have a self-contained Android closure')
    reuse = {'schema_version': 1, 'artifact_pin': pin, 'artifact_pin_sha256': digest(root / PIN),
             'runtime_lock': selected['lock'], 'runtime_lock_sha256': digest(lock_path),
             'runtime_receipt_sha256': digest(output / 'runtime-build.json')}
    (output / 'candidate-reuse.json').write_text(json.dumps(reuse, indent=2) + '\n')
    return reuse


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--archive', required=True, type=Path)
    parser.add_argument('--output', required=True, type=Path)
    parser.add_argument('--source', required=True, type=Path)
    args = parser.parse_args()
    prepare(args.archive, args.output, args.source)


if __name__ == '__main__':
    main()
