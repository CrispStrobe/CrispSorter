#!/usr/bin/env python3
"""Validate sibling release checkouts without rewriting committed Cargo sources."""
import argparse
import json
from pathlib import Path
import subprocess
import tomllib

from build_mobile_crispembed import check_source


def git(root, *arguments):
    return subprocess.check_output(['git', '-C', str(root), *arguments], text=True).strip()


def check(root, embed_commit, asr_ref, docx_ref, require_ocr_lock=False):
    root = root.resolve()
    lock = json.loads((root / 'scripts/crispembed-runtime-lock.json').read_text())
    mobile_lock = json.loads((root / 'scripts/ocr-mobile-runtime-lock.json').read_text())
    locks_agree = all(mobile_lock[key] == lock[key] for key in ['source_commit', 'ggml_commit'])
    if embed_commit != lock['source_commit']:
        raise ValueError('Release checkout and runtime pins must agree')
    if require_ocr_lock and not locks_agree:
        raise ValueError('Release and mobile qualification runtime pins must agree for OCR provenance')
    embed = root.parent / 'CrispEmbed'
    check_source(embed, lock)
    siblings = {'CrispEmbed': embed, 'CrispASR': root.parent / 'CrispASR',
                'crisp-docx': root.parent / 'crisp-docx'}
    for name, ref in [('CrispASR', asr_ref), ('crisp-docx', docx_ref)]:
        sibling = siblings[name]
        if (sibling.is_symlink() or not sibling.is_dir()
                or git(sibling, 'rev-parse', 'HEAD') != git(sibling, 'rev-parse', ref + '^{commit}')):
            raise ValueError('Sibling source does not match a configured checkout: ' + name)
    manifest_path = root / 'src-tauri/Cargo.toml'
    manifest = tomllib.loads(manifest_path.read_text())
    checked = []
    for name, dependency in manifest['dependencies'].items():
        if not isinstance(dependency, dict) or 'path' not in dependency:
            continue
        relative = Path(dependency['path'])
        if len(relative.parts) < 3 or relative.parts[:2] != ('..', '..') or relative.parts[2] not in siblings:
            continue
        target = manifest_path.parent / relative
        expected_root = siblings[relative.parts[2]]
        if (expected_root.is_symlink() or target.is_symlink()
                or not target.resolve().is_relative_to(expected_root.resolve())
                or not (target / 'Cargo.toml').is_file()):
            raise ValueError('Committed Cargo sibling dependency is unavailable: ' + name)
        checked.append(name)
    required = {'crispembed', 'crispembed-sys', 'crispasr', 'crispasr-sys',
                'crisp-docx-core', 'crisp-docx-llm', 'crisp-docx-align'}
    if not required.issubset(checked):
        raise ValueError('Expected committed sibling paths are missing or rewritten')
    return {'checked_dependencies': sorted(checked),
            'mobile_qualification_lock_matches_release': locks_agree}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--project', type=Path, default=Path.cwd())
    parser.add_argument('--embed-commit', required=True)
    parser.add_argument('--asr-ref', required=True)
    parser.add_argument('--docx-ref', required=True)
    parser.add_argument('--require-ocr-lock', action='store_true',
                        help='Require agreement before using the OCR application provenance recipe')
    args = parser.parse_args()
    print(json.dumps(check(args.project, args.embed_commit, args.asr_ref, args.docx_ref, args.require_ocr_lock), indent=2))
