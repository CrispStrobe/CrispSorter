#!/usr/bin/env python3
"""Prepare only a production-pinned, unpublished mobile OCR build environment."""
import argparse
import os
from pathlib import Path
import shutil
import subprocess
import sys

from build_mobile_crispembed import build_runtime
from check_mobile_release_layout import check as check_layout
from check_mobile_release_profile import profile
from check_release_versions import check_versions
from mobile_qualification_lock import select_lock
from mobile_staging_paths import directory, output_file
from prepare_ocr_cargo_env import prepare as prepare_cargo

RUNTIME_LOCK = 'scripts/ocr-mobile-runtime-lock.json'
PRODUCTION_LOCK = 'scripts/crispembed-runtime-lock.json'
TARGETS = {'android-arm64': 'aarch64-linux-android', 'ios-arm64': 'aarch64-apple-ios'}


def prepare(project, output, target, embed_commit, asr_ref, docx_ref, protoc, ndk=None):
    profile('ocr-draft')
    if output.is_symlink() or (output.exists() and not output.is_dir()):
        raise ValueError('Draft staging must be a regular directory')
    project, output = project.resolve(), output.resolve()
    if os.environ.get('CRISPSORTER_RELEASE_SELECTOR') != {'android-arm64': 'android_ocr_draft', 'ios-arm64': 'ios_ocr_draft'}.get(target):
        raise ValueError('Draft target differs from its explicit selector')
    if target not in TARGETS:
        raise ValueError('Only the explicit signed-device ARM64 draft targets are supported')
    protected = [project, *(project.parent / name for name in ['CrispEmbed', 'CrispASR', 'crisp-docx'])]
    if any(output == root or output.is_relative_to(root) or root.is_relative_to(output) for root in protected):
        raise ValueError('Draft staging must be outside the application and dependency source checkouts')
    resources = project / 'src-tauri/bin'
    if resources.is_symlink() or (resources.exists() and not resources.is_dir()):
        raise ValueError('Native resource staging must be a regular directory')
    production = select_lock(PRODUCTION_LOCK, project)
    runtime = select_lock(RUNTIME_LOCK, project)
    if production['source'] != runtime['source'] or runtime['source'] != embed_commit:
        raise ValueError('Draft runtime must use the existing production source pin')
    check_versions(project)
    check_layout(project, embed_commit, asr_ref, docx_ref, require_ocr_lock=True)
    directory(output)
    build_runtime(project.parent / 'CrispEmbed', output / 'runtime', target, 2, ndk, project / RUNTIME_LOCK)
    subprocess.run([sys.executable, str(project / 'scripts/prepare_mobile_ort.py'),
                    '--target', target, '--output', str(output / 'ort')], cwd=project, check=True)
    ort = output / 'ort'
    prepare_cargo(project, output / 'runtime/lib', protoc,
                  ort_lib_dir=ort / 'lib' if target.startswith('android-') else None,
                  ort_xcframework=ort / 'onnxruntime.xcframework' if target == 'ios-arm64' else None)
    resources.mkdir(exist_ok=True)
    shutil.copy2(output / 'runtime/runtime-build.json', output_file(resources, 'ocr-mobile-runtime.json'))
    for notice in (output / 'runtime/notices').iterdir():
        if not notice.is_file() or notice.is_symlink():
            raise ValueError('Native notices must be regular files')
        shutil.copy2(notice, output_file(resources, notice.name))
    for source, name in [('LICENSE', 'onnxruntime-LICENSE.txt'), ('ThirdPartyNotices.txt', 'onnxruntime-ThirdPartyNotices.txt')]:
        shutil.copy2(ort / 'notices' / source, output_file(resources, name))
    environment = {'OCR_DRAFT_ROOT': str(output), 'CRISPEMBED_SYS_LIB_DIR': str(output / 'runtime/lib'),
            'OCR_ORT_ROOT': str(ort), 'LICENSES_REQUIRE': '1',
            'LICENSES_CARGO_TARGET': TARGETS[target],
            'LICENSES_CARGO_FEATURES': 'ocr-mobile,tauri/custom-protocol',
            'LICENSES_CARGO_METADATA': str(output / 'cargo-metadata.json'),
            'CARGO_BUILD_JOBS': '2'}
    if target == 'android-arm64':
        environment['RUSTFLAGS'] = ('-Clink-arg=-landroid -Clink-arg=-llog -Clink-arg=-lOpenSLES '
                                    '-C link-arg=-Wl,-z,max-page-size=16384 -C link-arg=-Wl,-z,common-page-size=16384')
    return environment


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--project', type=Path, default=Path.cwd())
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--target', choices=TARGETS, required=True)
    parser.add_argument('--embed-commit', required=True)
    parser.add_argument('--asr-ref', required=True)
    parser.add_argument('--docx-ref', required=True)
    parser.add_argument('--protoc', type=Path, required=True)
    parser.add_argument('--ndk', type=Path)
    args = parser.parse_args()
    environment = prepare(args.project, args.output, args.target, args.embed_commit, args.asr_ref, args.docx_ref,
                          args.protoc, args.ndk)
    # These values reach nested Cargo calls through prepare_cargo's forced config.
    # GITHUB_ENV additionally makes them visible to later packaging/license steps.
    with Path(os.environ['GITHUB_ENV']).open('a') as stream:
        for name, value in environment.items():
            if '\n' in value or '\r' in value:
                raise ValueError('Draft environment value contains a newline')
            stream.write(f'{name}={value}\n')
