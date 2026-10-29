#!/usr/bin/env python3
"""Build a complete, source-pinned mobile CrispEmbed runtime in an empty directory.

This prepares native libraries only; it does not enable CrispEmbed in the app or
qualify the resulting build on a device. Use a clean, isolated checkout with
its pinned ggml submodule initialized. Requires Android NDK or Xcode as relevant.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys

from validate_android_native_libs import closure

LOCK = Path(__file__).with_name('crispembed-runtime-lock.json')
TARGETS = ('android-arm64', 'ios-arm64', 'ios-sim-arm64')


def run(args, **kwargs):
    print('+', ' '.join(map(str, args)), file=sys.stderr, flush=True)
    return subprocess.run(list(map(str, args)), check=True, **kwargs)


def git(repo, *args):
    return run(['git', '-C', repo, *args], stdout=subprocess.PIPE, text=True).stdout.strip()


def check_source(source, lock):
    if git(source, 'rev-parse', 'HEAD') != lock['source_commit']:
        raise ValueError('CrispEmbed checkout does not match the runtime lock')
    if git(source / 'ggml', 'rev-parse', 'HEAD') != lock['ggml_commit']:
        raise ValueError('ggml checkout does not match the runtime lock')
    if git(source, 'status', '--porcelain', '--untracked-files=all'):
        raise ValueError('CrispEmbed checkout must be clean, including its submodules')
    if git(source / 'ggml', 'status', '--porcelain', '--untracked-files=all'):
        raise ValueError('ggml checkout must be clean')


def configure_args(target, source, build, ndk=None):
    args = ['cmake', '-S', source, '-B', build, '-DCMAKE_BUILD_TYPE=Release',
            '-DBUILD_SHARED_LIBS=OFF', '-DCMAKE_POSITION_INDEPENDENT_CODE=ON',
            '-DGGML_NATIVE=OFF', '-DCRISPEMBED_NATIVE=OFF',
            '-DGGML_BLAS=OFF', '-DGGML_LLAMAFILE=OFF']
    # The pinned CMake file auto-discovers optional CrispASR siblings. Keep
    # this build's provenance limited to the two verified source commits.
    for component in ('AUDIO', 'PUNC', 'LID', 'TRUECASE'):
        args.append(f'-DCRISP_{component}_DIR={build / "disabled-optional" / component}')
    if target == 'android-arm64':
        if not ndk:
            raise ValueError('ANDROID_NDK_HOME or --ndk is required for Android')
        toolchain = ndk / 'build/cmake/android.toolchain.cmake'
        clang = ndk / 'toolchains/llvm/prebuilt/linux-x86_64/bin/clang'
        if not toolchain.is_file() or (sys.platform.startswith('linux') and not clang.is_file()):
            raise ValueError(f'Android NDK is incomplete: {ndk}')
        args += [f'-DCMAKE_TOOLCHAIN_FILE={toolchain}', '-DANDROID_ABI=arm64-v8a',
                 '-DANDROID_NATIVE_API_LEVEL=24', '-DANDROID_STL=c++_static',
                 '-DCRISPEMBED_BUILD_SHARED=ON', '-DGGML_METAL=OFF']
    else:
        sdk = 'iphonesimulator' if target == 'ios-sim-arm64' else 'iphoneos'
        sdk_path = run(['xcrun', '--sdk', sdk, '--show-sdk-path'], stdout=subprocess.PIPE, text=True).stdout.strip()
        cc = run(['xcrun', '--sdk', sdk, '-f', 'clang'], stdout=subprocess.PIPE, text=True).stdout.strip()
        cxx = run(['xcrun', '--sdk', sdk, '-f', 'clang++'], stdout=subprocess.PIPE, text=True).stdout.strip()
        args += ['-DCMAKE_SYSTEM_NAME=iOS', '-DCMAKE_OSX_DEPLOYMENT_TARGET=15.0',
                 '-DCMAKE_OSX_ARCHITECTURES=arm64', f'-DCMAKE_OSX_SYSROOT={sdk_path}',
                 f'-DCMAKE_C_COMPILER={cc}', f'-DCMAKE_CXX_COMPILER={cxx}',
                 '-DCRISPEMBED_BUILD_SHARED=OFF', '-DGGML_METAL=ON',
                 '-DGGML_METAL_EMBED_LIBRARY=ON']
    return args


def package_ios(build, libdir):
    archives = [build / 'libcrispembed-static.a', build / 'libcrispembed-core.a']
    archives += sorted((build / 'ggml').rglob('libggml*.a'))
    names = {p.name for p in archives if p.is_file()}
    required = {'libcrispembed-static.a', 'libcrispembed-core.a', 'libggml.a',
                'libggml-base.a', 'libggml-cpu.a', 'libggml-metal.a'}
    if not required.issubset(names):
        raise ValueError(f'iOS native build is missing static libraries: {sorted(required - names)}')
    if len(names) != len(archives):
        raise ValueError('iOS native build contains duplicate or missing archive names')
    output = libdir / 'libcrispembed.a'
    run(['xcrun', 'libtool', '-static', '-o', output, *archives])
    return [output]


def build_runtime(source, output, target, jobs, ndk):
    lock = json.loads(LOCK.read_text())
    source, output = source.resolve(), output.resolve()
    if output == source or source in output.parents or output in source.parents:
        raise ValueError('output must be outside the source checkout')
    if output.exists() and any(output.iterdir()):
        raise ValueError('output directory must be empty')
    check_source(source, lock)
    args = configure_args(target, source, output / 'build', ndk)
    libdir = output / 'lib'
    libdir.mkdir(parents=True, exist_ok=True)
    run(args)
    run(['cmake', '--build', output / 'build', '--target',
         'crispembed-shared' if target == 'android-arm64' else 'crispembed',
         '--config', 'Release', '--parallel', str(jobs)])
    if target == 'android-arm64':
        binary = output / 'build/libcrispembed.so'
        if not binary.is_file():
            raise ValueError(f'missing Android library: {binary}')
        # BUILD_SHARED_LIBS=OFF should statically incorporate ggml. If it does
        # not, reject the result rather than guess which similarly named ggml
        # binaries came from this same build.
        names = closure(binary.parent, 'arm64-v8a')
        if names != ['libcrispembed.so']:
            raise ValueError(f'Android build unexpectedly requires bundled libraries: {names}')
        shutil.copy2(binary, libdir / binary.name)
        artifacts = [libdir / binary.name]
    else:
        artifacts = package_ios(output / 'build', libdir)
    receipt = {'target': target, 'source_commit': lock['source_commit'],
               'ggml_commit': lock['ggml_commit'],
               'artifacts': {p.name: hashlib.sha256(p.read_bytes()).hexdigest() for p in artifacts}}
    (output / 'runtime-build.json').write_text(json.dumps(receipt, indent=2) + '\n')
    print(json.dumps(receipt, indent=2))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source', required=True, type=Path)
    parser.add_argument('--output', required=True, type=Path)
    parser.add_argument('--target', required=True, choices=TARGETS)
    parser.add_argument('--jobs', type=int, default=2)
    parser.add_argument('--ndk', type=Path, default=os.environ.get('ANDROID_NDK_HOME'))
    args = parser.parse_args()
    if args.jobs < 1:
        parser.error('--jobs must be positive')
    try:
        build_runtime(args.source, args.output, args.target, args.jobs,
                      Path(args.ndk).resolve() if args.ndk else None)
    except (ValueError, OSError, subprocess.CalledProcessError) as exc:
        print(f'mobile runtime build failed: {exc}', file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
