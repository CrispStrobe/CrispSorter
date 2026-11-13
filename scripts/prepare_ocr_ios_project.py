#!/usr/bin/env python3
"""Finish a generated Tauri iOS project for the optional native OCR profile.

Xcode must link the C++ runtime used by the static native library. Simulator
signing is disabled only for the simulator SDK; normal device signing remains.
"""
import argparse
import json
from pathlib import Path
import subprocess

MARKER = '        # Paired OCR static runtime\n'
ANCHOR = '        ENABLE_BITCODE: false\n'
SETTINGS = (MARKER +
            '        OTHER_LDFLAGS: "$(inherited) -lc++"\n' +
            '        "CODE_SIGNING_ALLOWED[sdk=iphonesimulator*]": NO\n')


def prepare(project, ort_framework=None, retain_native_symbols=False):
    settings = SETTINGS
    if ort_framework is not None:
        if not ort_framework.is_absolute() or not ort_framework.is_file() or ort_framework.name != 'onnxruntime':
            raise ValueError('Choose the qualified absolute ONNX Runtime static framework binary')
        # Rust staticlib metadata does not carry framework linkage into the
        # final Xcode link. Link the same qualified universal static archive.
        flags = '$(inherited) -lc++ -framework CoreML ' + json.dumps(str(ort_framework.resolve()))
        settings = settings.replace('        OTHER_LDFLAGS: "$(inherited) -lc++"\n',
                                    '        OTHER_LDFLAGS: ' + json.dumps(flags) + '\n')
    if retain_native_symbols:
        if ort_framework is None:
            raise ValueError('Symbol retention requires the exact qualified ONNX Runtime framework')
        # Keep already-linked C API names visible to the unchanged final IPA
        # provenance check. This changes post-link symbol stripping only: do
        # not force-load ORT, disable dead-code stripping, or change exports.
        settings += ('        # Signed OCR draft C API provenance\n'
                     '        STRIP_STYLE: debugging\n')
    source = project.read_text()
    if retain_native_symbols and MARKER not in source and any(
            line.strip().startswith('STRIP_STYLE:') for line in source.splitlines()):
        raise ValueError('Generated iOS template already defines strip style; review symbol retention explicitly')
    if MARKER not in source:
        if source.count(ANCHOR) != 1:
            raise ValueError('Unexpected generated iOS template; review native link settings before building')
        project.write_text(source.replace(ANCHOR, ANCHOR + settings))
    elif settings not in source:
        raise ValueError('Existing OCR iOS settings differ; review them before rebuilding')
    subprocess.run(['xcodegen', 'generate', '--spec', str(project.resolve())],
                   cwd=project.parent, check=True)
    projects = list(project.parent.glob('*.xcodeproj'))
    if len(projects) != 1:
        raise ValueError('Expected one generated iOS Xcode project')
    workspace = projects[0] / 'project.xcworkspace' / 'contents.xcworkspacedata'
    if not workspace.exists():
        workspace.parent.mkdir(parents=True, exist_ok=True)
        workspace.write_text('<?xml version="1.0" encoding="UTF-8"?>\n'
                             '<Workspace version="1.0"><FileRef location="self:" /></Workspace>\n')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('project', type=Path)
    parser.add_argument('--ort-framework', type=Path)
    parser.add_argument('--retain-native-symbols', action='store_true',
                        help='Draft-only post-link retention of C API names; final provenance remains mandatory')
    args = parser.parse_args()
    prepare(args.project, args.ort_framework, args.retain_native_symbols)
