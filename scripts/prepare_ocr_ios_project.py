#!/usr/bin/env python3
"""Finish a generated Tauri iOS project for the optional native OCR profile.

Xcode must link the C++ runtime used by the static native library. Simulator
signing is disabled only for the simulator SDK; normal device signing remains.
"""
import argparse
from pathlib import Path
import subprocess

MARKER = '        # Paired OCR static runtime\n'
ANCHOR = '        ENABLE_BITCODE: false\n'
SETTINGS = (MARKER +
            '        OTHER_LDFLAGS: "$(inherited) -lc++"\n' +
            '        "CODE_SIGNING_ALLOWED[sdk=iphonesimulator*]": NO\n')


def prepare(project):
    source = project.read_text()
    if MARKER not in source:
        if source.count(ANCHOR) != 1:
            raise ValueError('Unexpected generated iOS template; review native link settings before building')
        project.write_text(source.replace(ANCHOR, ANCHOR + SETTINGS))
    elif SETTINGS not in source:
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
    prepare(parser.parse_args().project)
