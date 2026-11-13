#!/usr/bin/env python3
"""Retain only original same-run iOS draft inputs; never attest app validation."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tomllib

from check_mobile_release_profile import profile
from mobile_native_provenance import BASE_NOTICES, SHEEN_NOTICES
from ocr_mobile_build_receipt import source_tree_identity
from prepare_ocr_cargo_env import MARKER
from prepare_signed_mobile_ocr_draft import RUNTIME_LOCK, PRODUCTION_LOCK

MIB = 1024**2
MAX_TOTAL = 768 * MIB
HEADERS = ('onnxruntime_session_options_config_keys.h', 'coreml_provider_factory.h',
           'cpu_provider_factory.h', 'onnxruntime_lite_custom_op.h',
           'onnxruntime_run_options_config_keys.h', 'onnxruntime_float16.h',
           'onnxruntime_ep_device_ep_metadata_keys.h', 'onnxruntime_cxx_inline.h',
           'onnxruntime_cxx_api.h', 'onnxruntime_ep_c_api.h', 'onnxruntime_c_api.h')
FRAMEWORK = 'onnxruntime.xcframework/ios-arm64/onnxruntime.framework/'
ORT_FILES = (tuple('include/' + name for name in HEADERS)
             + tuple(FRAMEWORK + 'Headers/' + name for name in HEADERS)
             + (FRAMEWORK + 'onnxruntime', FRAMEWORK + 'Info.plist',
                'onnxruntime.xcframework/Info.plist', 'notices/LICENSE', 'notices/ThirdPartyNotices.txt'))


def sha(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def regular(path, root, limit):
    if (path.is_symlink() or path.resolve() != path or not path.is_relative_to(root)
            or not path.is_file() or not 0 < path.stat().st_size <= limit):
        raise ValueError('Missing, unsafe, or oversized allowlisted input')
    return path


def read(path, root, limit=8*MIB):
    return json.loads(regular(path, root, limit).read_text())


def git(project, *args):
    return subprocess.check_output(['git', '-C', str(project), *args], text=True, timeout=30).strip()


def collect(project, staging):
    # The output stays separate from recorder-owned artifact/, including on failure.
    project, staging = project.absolute(), staging.absolute()
    if project.resolve() != project or staging.resolve() != staging or staging.is_relative_to(project):
        raise ValueError('Use original regular source and external staging directories')
    output = staging / 'replay-inputs'
    if output.exists() or output.is_symlink():
        raise ValueError('Do not replace retained replay evidence')
    output.mkdir()
    manifest = {'schema': 'crispsorter-ios-ocr-draft-replay-inputs-v1',
                'input_retention_status': 'incomplete', 'validation_status': 'not_performed',
                'qualification': 'Not established; original inputs only, not a signed-app validation receipt',
                'run': {name: os.environ.get(name) for name in ('GITHUB_RUN_ID', 'GITHUB_RUN_ATTEMPT', 'GITHUB_SHA')},
                'files': {}, 'total_bytes': 0}
    try:
        profile('ocr-draft')
        if os.environ.get('CRISPSORTER_RELEASE_SELECTOR') != 'ios_ocr_draft':
            raise ValueError('Only the explicit unpublished iOS OCR draft is supported')
        commit, tree = git(project, 'rev-parse', 'HEAD'), git(project, 'rev-parse', 'HEAD^{tree}')
        source = source_tree_identity(project)
        prep = read(staging / 'preparation.json', staging)
        layout = read(staging / 'layout.json', staging)
        lock = read(project / RUNTIME_LOCK, project)
        production = read(project / PRODUCTION_LOCK, project)
        if (prep.get('target') != 'ios-arm64' or prep.get('app_source') != commit
                or layout.get('app_source') != commit or layout.get('app_tree') != tree
                or prep.get('runtime_lock') != RUNTIME_LOCK
                or prep.get('runtime_lock_sha256') != sha(project / RUNTIME_LOCK)
                or prep.get('production_source') != lock['source_commit']
                or any(lock[key] != production[key] for key in ('source_commit', 'ggml_commit'))):
            raise ValueError('Preparation, layout, and production source binding differ')
        manifest.update(app_source=commit, app_tree=tree, source_identity=source,
                        runtime_lock_sha256=sha(project / RUNTIME_LOCK),
                        ort_lock_sha256=sha(project / 'scripts/ocr-mobile-ort-lock.json'))
        runtime = read(staging / 'runtime/runtime-build.json', staging)
        ort = read(staging / 'ort/ort-runtime.json', staging)
        ort_lock = read(project / 'scripts/ocr-mobile-ort-lock.json', project)
        if (runtime.get('target') != 'ios-arm64' or runtime.get('source_commit') != lock['source_commit']
                or runtime.get('ggml_commit') != lock['ggml_commit']
                or set(runtime.get('artifacts', {})) != {'libcrispembed.a'}
                or ort.get('target') != 'ios-arm64' or ort.get('kind') != 'ocr-mobile-ort-runtime'
                or ort.get('lock_sha256') != manifest['ort_lock_sha256']
                or set(ort.get('files', {})) != set(ORT_FILES)):
            raise ValueError('Original runtime/ORT identity or allowlisted inventory differs')
        pin = ort_lock['ios']
        if (ort.get('archive') != {key: pin[key] for key in ('url', 'filename', 'bytes', 'sha256')}
                or ort['files'][FRAMEWORK + 'onnxruntime'] != pin['libraries']['ios-arm64']
                or ort['files']['include/onnxruntime_c_api.h']['sha256'] != ort_lock['header_sha256']):
            raise ValueError('ORT framework or header differs from committed binary pin')
        config = regular(project / '.cargo/config.toml', project, MIB)
        parsed = tomllib.loads(config.read_text())
        env = parsed.get('env', {})
        expected = {'CRISPEMBED_SYS_LIB_DIR': str(staging / 'runtime/lib'),
                    'PROTOC': env.get('PROTOC', {}).get('value'),
                    'ORT_IOS_XCFWK_LOCATION': str(staging / 'ort/onnxruntime.xcframework')}
        if not isinstance(expected['PROTOC'], str) or not Path(expected['PROTOC']).is_absolute():
            raise ValueError('Missing original forced protoc path')
        canonical = '\n\n' + MARKER + '\n' + '\n'.join(
            f'[env.{name}]\nvalue = {json.dumps(value, ensure_ascii=False)}\nforce = true\n'
            for name, value in expected.items())
        if config.read_text() != canonical or parsed != {'env': {k: {'value': v, 'force': True} for k, v in expected.items()}}:
            raise ValueError('Retain only the exact generated forced-path Cargo configuration')
        metadata = read(staging / 'cargo-metadata.json', staging, 64*MIB)
        if (metadata.get('kind') != 'locked-native-license-metadata' or metadata.get('source_commit') != commit
                or metadata.get('target') != 'aarch64-apple-ios' or metadata.get('default_features') is not False
                or metadata.get('features') != ['ocr-mobile', 'tauri/custom-protocol']
                or any(metadata.get(key) != sha(project / path) for key, path in (
                    ('cargo_lock_sha256', 'Cargo.lock'), ('workspace_manifest_sha256', 'Cargo.toml'),
                    ('application_manifest_sha256', 'src-tauri/Cargo.toml')))):
            raise ValueError('Generated Cargo graph source/target binding differs')
        # No receipt-supplied path is used as a copy source. Every path is fixed here.
        inventory = [('staging/' + name, staging / name, staging, limit, digest)
                     for name, limit, digest in [
                         ('preparation.json', 8*MIB, None), ('layout.json', 8*MIB, None),
                         ('runtime/runtime-build.json', 8*MIB, None), ('ort/ort-runtime.json', 8*MIB, None),
                         ('cargo-metadata.json', 64*MIB, None),
                         ('runtime/lib/libcrispembed.a', 512*MIB, runtime['artifacts']['libcrispembed.a'])]]
        notices = dict(BASE_NOTICES)
        if 'sheenbidi' in lock.get('native_components', {}):
            notices.update(SHEEN_NOTICES)
        records = runtime.get('native_provenance', {}).get('notices', {})
        if set(records) != set(notices):
            raise ValueError('Original native notice inventory differs')
        for name in notices:
            item = regular(staging / 'runtime/notices' / name, staging, MIB)
            if item.stat().st_size != records[name]['bytes']:
                raise ValueError('Original native notice length differs from receipt')
        for name in ORT_FILES:
            item = regular(staging / 'ort' / name, staging, 64*MIB)
            if item.stat().st_size != ort['files'][name]['bytes']:
                raise ValueError('Original ORT input length differs from receipt')
        inventory += [('staging/runtime/notices/' + name, staging / 'runtime/notices' / name, staging, MIB,
                       records[name]['sha256']) for name in notices]
        inventory += [('staging/ort/' + name, staging / 'ort' / name, staging, 64*MIB,
                       ort['files'][name]['sha256']) for name in ORT_FILES]
        inventory += [('source/' + name, project / name, project, 8*MIB, None)
                      for name in ('.cargo/config.toml', 'static/licenses.json')]
        for name, original, root, limit, expected_sha in inventory:
            regular(original, root, limit)
            count = original.stat().st_size
            if manifest['total_bytes'] + count > MAX_TOTAL:
                raise ValueError('Replay input bundle exceeds total byte bound')
            destination = output / name
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(original, destination)
            digest = sha(destination)
            if (destination.stat().st_size != count or sha(original) != digest
                    or expected_sha is not None and digest != expected_sha):
                destination.unlink()
                raise ValueError('Original input changed or differs from its receipt')
            manifest['files'][name] = {'bytes': count, 'sha256': digest}
            manifest['total_bytes'] += count
        version = prep.get('version', '')
        if not isinstance(version, str) or not version or any(c not in '0123456789.-abcdefghijklmnopqrstuvwxyz' for c in version):
            raise ValueError('Unexpected prepared application version')
        signed = staging / 'artifact' / f'CrispSorter_{version}_ios-arm64-ocr-draft-unqualified.ipa'
        regular(signed, staging, 512*MIB)
        manifest['signed_artifact'] = {'filename': signed.name, 'bytes': signed.stat().st_size,
                                       'sha256': sha(signed), 'validation_status': 'not_asserted'}
        if (git(project, 'rev-parse', 'HEAD') != commit
                or source_tree_identity(project) != source):
            raise ValueError('Application source changed during input retention')
        manifest['input_retention_status'] = 'complete'
        return manifest
    except Exception as error:
        # Do not copy raw exception text: external tools may include paths/credentials.
        manifest['failure_type'] = type(error).__name__
        raise
    finally:
        (output / 'replay-inputs.json').write_text(json.dumps(manifest, indent=2) + '\n')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--project', type=Path, default=Path.cwd())
    parser.add_argument('--staging', type=Path, required=True)
    args = parser.parse_args()
    result = collect(args.project, args.staging)
    print('Retained original draft inputs (not app qualification):', result['total_bytes'], 'bytes')
