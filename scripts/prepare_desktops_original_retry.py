#!/usr/bin/env python3
"""Exact D204 desktop retry: test-only overlay, restored production bytes, scoped uploads."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import subprocess
import sys

from check_release_target import validate_release_target
from mobile_draft_safe_cli import SafeArgumentParser, run as safe_run

TOOLING = Path(__file__).resolve().parents[1]
PIN = 'scripts/desktops-original-d204-lock.json'
OCR = 'src-tauri/src/extractors/ocr_crispembed.rs'
SELECTORS = {'linux_original_d204': 'linux-x86_64', 'windows_original_d204': 'windows-x86_64',
             'macos_original_d204': 'macos-arm64'}
INPUTS = {'Cargo.toml', 'Cargo.lock', 'src-tauri/Cargo.toml', 'src-tauri/tauri.conf.json',
          'package.json', 'package-lock.json', 'scripts/crispembed-runtime-lock.json', OCR}

def sha(data):
    return hashlib.sha256(data).hexdigest()

def git(root, *args):
    return subprocess.check_output(['git', '-C', str(root), *args], timeout=30).decode().strip()

def committed(root, name):
    return subprocess.check_output(['git', '-C', str(root), 'show', 'HEAD:' + name], timeout=30)

def read(root, name):
    path = root / name
    if path.is_symlink() or not path.is_file() or path.resolve() != path.absolute():
        raise ValueError('Regular source-contained file required')
    return path.read_bytes()

def overlay(original):
    result = original
    for function in ['qwen_internal_noncommercial_smoke_live', 'ocr_task_presets_live']:
        pattern = (rb'    #\[cfg\(feature = "crispembed"\)\]\n(?=    #\[test\]\n'
                   rb'    #\[ignore[^\n]*\]\n    fn ' + function.encode() + rb'\(\))')
        result, count = re.subn(pattern, lambda match: match[0] + b'    #[cfg(feature = "sidecars")]\n', result)
        if count != 1:
            raise ValueError('Exact live-test overlay preimage required')
    return result

def relocated_manifest(original):
    text = original.decode()
    for sibling, middle in [('CrispEmbed', ''), ('CrispASR', ''), ('crisp-docx', 'crates/')]:
        text = re.sub(r'path\s*=\s*"\.\./\.\./' + sibling + '/' + middle + r'([\w-]+)"',
                      lambda m: 'path = "../_sibling/' + sibling + '/' + middle + m[1] + '"', text)
    return text.encode()

def recipes(workflow):
    section = workflow.split('  release-required:\n', 1)[1].split('    runs-on:', 1)[0]
    blocks = re.split(r'^          - ', section, flags=re.M)[1:]
    result = {}
    fields = 'platform|qualification_platform|rust_target|tauri_args|llama_asset|sidecar_name|crispasr_lib_asset|crispembed_lib_asset|pdfium_asset|pdfium_lib_glob'
    for block in blocks:
        values = dict(re.findall(r'^\s*(' + fields + r'):\s*(.+)$', block, re.M))
        values = {key: value.strip().strip("'") for key, value in values.items()}
        result[values['qualification_platform']] = values
    refs = dict(re.findall(r'^  (CRISPEMBED_REPO|CRISPEMBED_REF|CRISPEMBED_COMMIT|CRISPASR_REPO|CRISPASR_REF|CRISPDOCX_REPO|CRISPDOCX_REF):\s*([^#\n]+)', workflow, re.M))
    return result, {key: value.strip() for key, value in refs.items()}

def allowed_names(platform, version):
    base = 'CrispSorter_' + version
    packages = {
        'linux-x86_64': {base + '_amd64.deb'},
        'windows-x86_64': {base + '_x64-windows-portable.zip', base + '_x64-windows-portable-unsigned.zip'},
        'macos-arm64': {base + '_aarch64.dmg', base + '_aarch64-unsigned.dmg',
                        'CrispSorter_aarch64.app.tar.gz', 'CrispSorter_aarch64-unsigned.app.tar.gz'},
    }[platform]
    return packages | {base + '_' + platform + suffix for suffix in ['.receipt.json', '.sha256']}

def draft(repo, pin):
    if repo != 'CrispStrobe/CrispSorter':
        raise ValueError('Pinned repository required')
    identity = json.loads(subprocess.check_output(['gh', 'release', 'view', pin['tag'], '--repo', repo,
                                                  '--json', 'databaseId'], timeout=60))
    release_id = identity.get('databaseId')
    if type(release_id) is not int or release_id <= 0:
        raise ValueError('Existing draft identity required')
    value = json.loads(subprocess.check_output(['gh', 'api', f"repos/{repo}/releases/{release_id}"], timeout=60))
    if (value.get('id') != release_id or value.get('tag_name') != pin['tag']
            or value.get('draft') is not True or value.get('prerelease') is not False
            or value.get('target_commitish') != pin['source']):
        raise ValueError('Exact existing original draft required')
    validate_release_target(repo, pin['tag'], pin['source'])
    assets = value.get('assets')
    if (not isinstance(assets, list) or len(assets) >= 100
            or not all(isinstance(a, dict) and isinstance(a.get('name'), str)
                       and type(a.get('id')) is int and type(a.get('size')) is int for a in assets)
            or len({a['name'] for a in assets}) != len(assets)):
        raise ValueError('Unambiguous bounded draft assets required')
    return sha(str(release_id).encode()), {a['name']: {key: a.get(key) for key in ['id', 'name', 'size', 'digest']} for a in assets}

def verify(project, repo, env, overlay_active=False, runtime=False):
    selector = env.get('CRISPSORTER_RELEASE_SELECTOR')
    if (env.get('GITHUB_EVENT_NAME') != 'workflow_dispatch' or selector not in SELECTORS
            or env.get('CRISPSORTER_RELEASE_PUBLISH') != 'false'
            or env.get('CRISPSORTER_RELEASE_MAC_APPSTORE') != 'false'
            or env.get('CRISPSORTER_MOBILE_RELEASE_PROFILE') != 'standard'):
        raise ValueError('Explicit unpublished standard desktop retry required')
    workflow = env.get('GITHUB_WORKFLOW_SHA', '')
    if not re.fullmatch('[0-9a-f]{40}', workflow) or git(TOOLING, 'rev-parse', 'HEAD') != workflow:
        raise ValueError('Exact corrected workflow identity required')
    if git(TOOLING, 'diff', '--name-only', 'HEAD', '--'):
        raise ValueError('Corrected tooling has tracked modifications')
    pin_data = committed(TOOLING, PIN)
    if read(TOOLING, PIN) != pin_data:
        raise ValueError('Committed retry pin required')
    pin = json.loads(pin_data)
    if (pin.get('schema') != 1 or pin.get('source') != 'd204522ad7902eaa6494f92dd740f8c469dc1acc'
            or pin.get('tree') != '587b71a0590fb426551de0eee1949b953e188e95'
            or pin.get('version') != '0.15.2'
            or pin.get('tag') != 'v0.15.2'):
        raise ValueError('Reviewed original retry identity required')
    if set(pin.get('files', {})) != INPUTS:
        raise ValueError('Complete original retry input pins required')
    if git(project, 'rev-parse', 'HEAD') != pin['source'] or git(project, 'rev-parse', 'HEAD^{tree}') != pin['tree']:
        raise ValueError('Exact original app checkout required')
    modified = git(project, 'diff', '--name-only', 'HEAD', '--').splitlines()
    permitted = {'src-tauri/Cargo.toml', 'static/licenses.json'} | ({OCR} if overlay_active else set())
    if set(modified) - permitted:
        raise ValueError('Unexpected tracked source modification')
    changes = []
    for name, expected in pin['files'].items():
        original, actual = committed(project, name), read(project, name)
        if sha(original) != expected:
            raise ValueError('Original input pin mismatch')
        possibilities = [original]
        if name == 'src-tauri/Cargo.toml':
            possibilities.append(relocated_manifest(original))
        if name == OCR and overlay_active:
            possibilities = [overlay(original)]
        if actual not in possibilities:
            raise ValueError('Effective input is not the exact admitted transformation')
        if actual != original:
            changes.append({'path': name, 'original_sha256': expected, 'effective_sha256': sha(actual)})
    if 'static/licenses.json' in modified:
        data = read(project, 'static/licenses.json')
        if not 0 < len(data) <= 8 * 1024 * 1024 or not isinstance(json.loads(data), dict):
            raise ValueError('Bounded generated license inventory required')
        changes.append({'path': 'static/licenses.json', 'effective_sha256': sha(data), 'kind': 'generated license inventory'})
    original_recipes, refs = recipes(committed(project, '.github/workflows/release.yml').decode())
    corrected_recipes, corrected_refs = recipes(committed(TOOLING, '.github/workflows/release.yml').decode())
    if original_recipes != corrected_recipes or refs != corrected_refs:
        raise ValueError('Original desktop/native recipe changed')
    platform = SELECTORS[selector]
    dependencies = pin['dependency_sources']
    if runtime:
        expected_os = {'linux-x86_64': 'Linux', 'windows-x86_64': 'Windows', 'macos-arm64': 'macOS'}[platform]
        if env.get('RUNNER_OS') != expected_os:
            raise ValueError('Original retry platform differs from runner')
        for sibling, expected in dependencies.items():
            checkout = project / '_sibling' / sibling
            if git(checkout, 'rev-parse', 'HEAD') != expected or git(checkout, 'diff', '--name-only', 'HEAD', '--'):
                raise ValueError('Original native dependency source changed')
    draft_identity, assets = draft(repo, pin)
    return {'schema': 1, 'kind': 'original-d204-desktop-retry', 'app_source': pin['source'], 'app_tree': pin['tree'],
            'workflow_source': workflow, 'workflow_tree': git(TOOLING, 'rev-parse', 'HEAD^{tree}'),
            'platform': platform, 'version': pin['version'], 'draft_identity_sha256': draft_identity,
            'pin_sha256': sha(pin_data), 'recipe': original_recipes[platform], 'source_adjustments': changes,
            'assets': assets, 'dependency_sources': dependencies}

def write_proof(path, value):
    if path.absolute() != path.resolve() or path.is_symlink():
        raise ValueError('Regular proof path required')
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2) + '\n')

def test_commands(proof):
    args = shlex.split(proof['recipe']['tauri_args'])
    features = args[args.index('--features') + 1]
    command = ['cargo', 'test', '--locked', '-p', 'crispsorter', '--release', '--lib']
    if proof['platform'] == 'macos-arm64':
        command += ['--target', proof['recipe']['rust_target']]
    return command + ['--features', features, '--', '--test-threads=2']

def run_tests(project, repo, env, output):
    proof = verify(project, repo, env, runtime=True)
    if output.exists():
        raise ValueError('Fresh test proof path required')
    original = read(project, OCR)
    corrected = overlay(original)
    # Compare full bytes with the separately committed source fix; arbitrary
    # corrected source or changes outside those two cfg attributes are refused.
    if read(TOOLING, OCR) != committed(TOOLING, OCR) or corrected != committed(TOOLING, OCR):
        raise ValueError('Committed tooling must contain only the exact test overlay')
    proof['test_overlay'] = {'path': OCR, 'original_sha256': sha(original), 'test_sha256': sha(corrected),
                             'transformation': 'Two sidecars cfg attributes on Git-backed ignored live tests only'}
    proof.update(tests_status='failed', production_ocr_restored=False)
    write_proof(output, proof)
    code = None
    try:
        (project / OCR).write_bytes(corrected)
        verify(project, repo, env, overlay_active=True, runtime=True)
        command = test_commands(proof)
        proof['test_command'] = command
        code = subprocess.run(command, cwd=project, check=False).returncode
        proof['test_exit_code'] = code
        if code:
            raise ValueError('Native regression tests failed')
        proof['tests_status'] = 'passed'
    finally:
        # Restoration is unconditional; even a failed test must not leave the
        # overlay available for a subsequent production compile.
        try:
            (project / OCR).write_bytes(original)
            proof['production_ocr_restored'] = read(project, OCR) == committed(project, OCR) == original
        except OSError:
            proof.update(tests_status='failed', restoration_failure='restore-failed')
        finally:
            write_proof(output, proof)
        if not proof['production_ocr_restored']:
            raise ValueError('Original production OCR source was not restored') from None
    if not proof['production_ocr_restored']:
        raise ValueError('Original production OCR source was not restored')
    return proof

def checked_proof(project, repo, env, output):
    proof = verify(project, repo, env, runtime=True)
    if output.is_symlink() or not output.is_file() or output.stat().st_size > 2 * 1024 * 1024:
        raise ValueError('Bounded test proof required')
    tests = json.loads(output.read_text())
    for key in ['app_source', 'app_tree', 'workflow_source', 'workflow_tree', 'platform', 'pin_sha256', 'recipe', 'draft_identity_sha256']:
        if tests.get(key) != proof[key]:
            raise ValueError('Test proof identity changed')
    if (tests.get('tests_status') != 'passed' or tests.get('test_exit_code') != 0
            or tests.get('production_ocr_restored') is not True or tests.get('test_command') != test_commands(proof)
            or tests.get('test_overlay', {}).get('original_sha256') != sha(committed(project, OCR))
            or tests.get('test_overlay', {}).get('test_sha256') != sha(overlay(committed(project, OCR)))):
        raise ValueError('Restored successful test-overlay proof required')
    return proof, tests

def upload(project, repo, env, output, receipt_path):
    proof, tests = checked_proof(project, repo, env, output)
    platform, version = proof['platform'], proof['version']
    expected_receipt = f'CrispSorter_{version}_{platform}.receipt.json'
    if receipt_path != project / expected_receipt or receipt_path.is_symlink():
        raise ValueError('Exact selected-platform receipt required')
    receipt = json.loads(read(project, expected_receipt))
    if (receipt.get('schema') != 'crispsorter-release-receipt-v1' or receipt.get('app_source') != proof['app_source']
            or receipt.get('version') != version or receipt.get('platform') != platform
            or receipt.get('dependency_sources') != proof['dependency_sources']
            or receipt.get('runtime_lock') != json.loads(committed(project, 'scripts/crispembed-runtime-lock.json'))):
        raise ValueError('Original selected-platform package receipt required')
    artifacts = receipt.get('artifacts', [])
    if len(artifacts) != (2 if platform == 'macos-arm64' else 1):
        raise ValueError('Exact package count required')
    if (not all(isinstance(entry, dict) for entry in artifacts)
            or len({entry.get('filename') for entry in artifacts}) != len(artifacts)):
        raise ValueError('Unique package names required')
    if platform == 'macos-arm64' and (sum(a.get('filename', '').endswith('.dmg') for a in artifacts) != 1
            or sum(a.get('filename', '').endswith('.app.tar.gz') for a in artifacts) != 1):
        raise ValueError('Exactly one DMG and one app archive required')
    if platform == 'macos-arm64' and len({'-unsigned' in a['filename'] for a in artifacts}) != 1:
        raise ValueError('Mac app archive and DMG signing labels differ')
    paths, checksum_rows = [], []
    for entry in artifacts:
        name = entry.get('filename')
        if name not in allowed_names(platform, version) or not name.endswith(('.deb', '.zip', '.dmg', '.app.tar.gz')):
            raise ValueError('Package is outside the selected platform')
        if platform == 'windows-x86_64':
            matches = [project / name]
        else:
            folder = 'deb' if platform == 'linux-x86_64' else ('dmg' if name.endswith('.dmg') else 'macos')
            roots = [project / 'target/release/bundle', project / 'target' / proof['recipe']['rust_target'] / 'release/bundle']
            matches = [root / folder / name for root in roots if (root / folder / name).is_file()]
        if len(matches) != 1 or matches[0].is_symlink() or not matches[0].resolve().is_relative_to(project):
            raise ValueError('Unique source-contained final package required')
        path = matches[0]
        with path.open('rb') as stream:
            digest = hashlib.file_digest(stream, 'sha256').hexdigest()
        if path.stat().st_size != entry.get('bytes') or digest != entry.get('sha256'):
            raise ValueError('Final package differs from verified receipt')
        paths.append(path)
        checksum_rows.append(f"{entry['sha256']}  {name}\n")
    checksum_name = f'CrispSorter_{version}_{platform}.sha256'
    if read(project, checksum_name).decode() != ''.join(checksum_rows):
        raise ValueError('Selected-platform checksum file differs')
    protected = {name: value for name, value in proof['assets'].items() if name not in allowed_names(platform, version)}
    receipt['workflow_source'] = proof['workflow_source']
    receipt['desktop_original_retry'] = {**{key: value for key, value in proof.items() if key != 'assets'},
        'test_overlay': tests['test_overlay'], 'tests_status': tests['tests_status'],
        'test_command': tests['test_command'], 'test_exit_code': tests['test_exit_code'],
        'test_proof_sha256': sha(output.read_bytes()), 'production_ocr_restored': True, 'protected_asset_names': sorted(protected)}
    receipt_path.write_text(json.dumps(receipt, indent=2) + '\n')
    # Recheck source, exact draft and all unrelated asset identities immediately
    # before uploading. No delete operation exists in this route.
    refreshed = verify(project, repo, env, runtime=True)
    if refreshed['draft_identity_sha256'] != proof['draft_identity_sha256']:
        raise ValueError('Original draft identity changed')
    latest = refreshed['assets']
    if any(latest.get(name) != value for name, value in protected.items()):
        raise ValueError('Protected draft assets changed')
    subprocess.run(['gh', 'release', 'upload', 'v' + version, '--repo', repo, '--clobber',
                    *map(str, paths), str(receipt_path), str(project / checksum_name)], check=True)
    refreshed = verify(project, repo, env, runtime=True)
    if refreshed['draft_identity_sha256'] != proof['draft_identity_sha256']:
        raise ValueError('Original draft identity changed during upload')
    after = refreshed['assets']
    if any(after.get(name) != value for name, value in protected.items()):
        raise ValueError('Protected draft assets changed during scoped upload')
    for path in [*paths, receipt_path, project / checksum_name]:
        with path.open('rb') as stream:
            digest = hashlib.file_digest(stream, 'sha256').hexdigest()
        asset = after.get(path.name, {})
        if asset.get('size') != path.stat().st_size or asset.get('digest') != 'sha256:' + digest:
            raise ValueError('Uploaded final asset digest differs')

def main():
    parser = SafeArgumentParser(description=__doc__)
    parser.add_argument('--repo', required=True)
    parser.add_argument('--project', type=Path, default=Path.cwd())
    parser.add_argument('--phase', choices=['prepare', 'test', 'check', 'upload'], default='prepare')
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--receipt', type=Path)
    args = parser.parse_args()
    project = args.project.resolve()
    if args.phase == 'prepare':
        if args.output.exists():
            raise ValueError('Fresh preflight proof required')
        write_proof(args.output, verify(project, args.repo, os.environ))
    elif args.phase == 'test':
        run_tests(project, args.repo, os.environ, args.output)
    elif args.phase == 'check':
        checked_proof(project, args.repo, os.environ, args.output)
    elif args.phase == 'upload':
        if args.receipt is None:
            raise ValueError('Exact receipt required')
        upload(project, args.repo, os.environ, args.output, args.receipt.absolute())
    print('Original desktop retry checks passed; no public or mobile release authorization added')

if __name__ == '__main__':
    sys.exit(safe_run(main))
