#!/usr/bin/env python3
"""Verify one unpublished Windows retry with distinct app/workflow identities."""
import argparse
import copy
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import tomllib

from check_release_target import validate_release_target

TOOLING = Path(__file__).resolve().parents[1]
PIN = 'scripts/windows-original-retry-lock.json'
SELECTOR = 'windows_original_b739'


def git(root, *args):
    return subprocess.check_output(['git', '-C', str(root), *args], text=True).strip()


def digest(data):
    return hashlib.sha256(data).hexdigest()


def committed(root, relative):
    return subprocess.check_output(['git', '-C', str(root), 'show', 'HEAD:' + relative])


def normalized_manifest(data):
    def normalize(value):
        if isinstance(value, dict):
            return {key: normalize(item) for key, item in value.items()}
        if isinstance(value, list):
            return [normalize(item) for item in value]
        if isinstance(value, str):
            for sibling in ('CrispEmbed', 'CrispASR', 'crisp-docx'):
                prefix = '../_sibling/' + sibling + '/'
                if value.startswith(prefix):
                    return '../../' + sibling + '/' + value[len(prefix):]
        return value
    return normalize(tomllib.loads(data.decode()))


def windows_recipe(workflow):
    start = workflow.index('          - &windows_release\n')
    end = workflow.index('          - platform:', start)
    matrix = dict(re.findall(r'^\s+(platform|qualification_platform|rust_target|tauri_args|llama_asset|sidecar_name|crispasr_lib_asset|crispembed_lib_asset|pdfium_asset|pdfium_lib_glob):\s*(.+)$', workflow[start:end], re.M))
    refs = dict(re.findall(r'^  (CRISPEMBED_REPO|CRISPEMBED_REF|CRISPEMBED_COMMIT|CRISPASR_REPO|CRISPASR_REF|CRISPDOCX_REPO|CRISPDOCX_REF):\s*([^#\n]+)', workflow, re.M))
    return {'matrix': {k: v.strip() for k, v in matrix.items()},
            'native_refs': {k: v.strip() for k, v in refs.items()}}


def lock_metadata_graph(data):
    metadata = tomllib.loads(data.decode())
    graph = copy.deepcopy(metadata)
    packages = graph.get('package', [])
    roots = [package for package in packages if package.get('name') == 'crispsorter']
    if len(roots) != 1 or roots[0].get('version') != '0.15.1':
        raise ValueError('Canonical lock requires the exact original root package')
    dependencies = roots[0].get('dependencies')
    if (not isinstance(dependencies, list) or not all(isinstance(item, str) for item in dependencies)
            or len(dependencies) != len(set(dependencies))):
        raise ValueError('Canonical lock root dependency inventory is invalid')
    # Every other parsed field and every other package/dependency array stays
    # untouched. This is complete lock metadata equality, not just a package set.
    roots[0]['dependencies'] = sorted(dependencies)
    return graph


def verify_cargo_lock(original, actual, pin, phase):
    policy = pin['post_build_cargo_lock']
    original_sha, effective_sha = digest(original), digest(actual)
    if original_sha != pin['files']['Cargo.lock'] or original_sha != policy['original_sha256']:
        raise ValueError('Original Cargo.lock differs from its exact source pin')
    original_graph = lock_metadata_graph(original)
    graph_sha = digest(json.dumps(original_graph, sort_keys=True, separators=(',', ':')).encode())
    if graph_sha != policy['lock_metadata_graph_sha256']:
        raise ValueError('Original full lock metadata/graph differs from the reviewed pin')
    if phase == 'prepare' and actual != original:
        raise ValueError('Pre-build Cargo.lock must match exact original bytes')
    if actual != original:
        # Independently reproduced Cargo output has exactly this root-array
        # reorder. Reconstruct and verify those exact bytes as well as its hash.
        before = b' "rustls",\n "rustls-native-certs",\n "rten",\n "rusqlite",\n'
        after = b' "rten",\n "rusqlite",\n "rustls",\n "rustls-native-certs",\n'
        root_start = original.index(b'[[package]]\nname = "crispsorter"\n')
        root_end = original.index(b'\n[[package]]', root_start + 1)
        block = original[root_start:root_end]
        if block.count(before) != 1:
            raise ValueError('Original root-array reorder does not match its reviewed source')
        canonical = original[:root_start] + block.replace(before, after) + original[root_end:]
        if (effective_sha != policy['effective_sha256'] or actual != canonical
                or lock_metadata_graph(actual) != original_graph):
            raise ValueError('Post-build Cargo.lock is not the exact graph-preserving canonical reorder')
    return {'original_sha256': original_sha, 'effective_sha256': effective_sha,
            'lock_metadata_graph_sha256': graph_sha, 'lock_metadata_graph_equal': True,
            'canonical_dependency_order_applied': actual != original}


def lookup_draft(repo, pin):
    if not re.fullmatch(r'[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+', repo):
        raise ValueError('Invalid repository name')
    result = subprocess.check_output(['gh', 'api', '--paginate', '--slurp',
                                      f'repos/{repo}/releases?per_page=100'], text=True, encoding='utf-8')
    pages = json.loads(result)
    if not isinstance(pages, list) or not all(isinstance(page, list) for page in pages):
        raise ValueError('Release listing must contain all paginated release pages')
    releases = [release for page in pages for release in page]
    if not all(isinstance(release, dict) for release in releases):
        raise ValueError('Release listing has invalid entries')
    matches = [release for release in releases if release.get('tag_name') == pin['tag']]
    if len(matches) != 1:
        raise ValueError('Original Windows retry requires exactly one existing release for the pinned tag')
    draft = matches[0]
    if (draft.get('draft') is not True or draft.get('prerelease') is not False
            or draft.get('target_commitish') != pin['source_commit']):
        raise ValueError('Original Windows retry requires the exact existing unpublished draft and target')
    validate_release_target(repo, pin['tag'], pin['source_commit'])
    return {'tag': pin['tag'], 'source': pin['source_commit']}


def verify(project, tooling, repo, env, phase='prepare'):
    if (env.get('GITHUB_EVENT_NAME') != 'workflow_dispatch'
            or env.get('CRISPSORTER_RELEASE_SELECTOR') != SELECTOR
            or env.get('CRISPSORTER_RELEASE_PUBLISH') != 'false'
            or env.get('CRISPSORTER_RELEASE_MAC_APPSTORE') != 'false'
            or env.get('CRISPSORTER_MOBILE_RELEASE_PROFILE') != 'standard'):
        raise ValueError('Original Windows retry requires its explicit manual selector, standard profile, publish=false and mac_appstore=false')
    workflow = env.get('GITHUB_WORKFLOW_SHA', '')
    if not re.fullmatch('[0-9a-f]{40}', workflow) or git(tooling, 'rev-parse', 'HEAD') != workflow:
        raise ValueError('Corrected workflow checkout does not match GITHUB_WORKFLOW_SHA')
    pin_bytes = committed(tooling, PIN)
    if (tooling / PIN).read_bytes() != pin_bytes:
        raise ValueError('Windows retry lock differs from its committed bytes')
    pin = json.loads(pin_bytes)
    if (git(project, 'rev-parse', 'HEAD') != pin['source_commit']
            or git(project, 'rev-parse', 'HEAD^{tree}') != pin['source_tree']):
        raise ValueError('Windows retry app checkout differs from the exact original source')
    if phase == 'prepare' and git(project, 'status', '--porcelain', '--untracked-files=no'):
        raise ValueError('Original Windows app checkout must have clean tracked sources before preparation')
    lock_proof = None
    for name, expected in pin['files'].items():
        data = committed(project, name)
        actual = (project / name).read_bytes()
        if name == 'Cargo.lock':
            lock_proof = verify_cargo_lock(data, actual, pin, phase)
            equivalent = True
        else:
            equivalent = (actual == data if phase == 'prepare' or name != 'src-tauri/Cargo.toml'
                          else normalized_manifest(actual) == normalized_manifest(data))
        if digest(data) != expected or committed(tooling, name) != data or not equivalent:
            raise ValueError('Windows retry version/dependency input changed: ' + name)
    original = committed(project, '.github/workflows/release.yml').decode()
    corrected = committed(tooling, '.github/workflows/release.yml').decode()
    if windows_recipe(original) != windows_recipe(corrected):
        raise ValueError('Corrected Windows workflow changed the original features/native recipe')
    lookup_draft(repo, pin)
    return {'schema_version': 1, 'kind': 'windows-original-source-retry',
            'app_source': pin['source_commit'], 'app_tree': pin['source_tree'],
            'workflow_source': workflow, 'workflow_tree': git(tooling, 'rev-parse', 'HEAD^{tree}'),
            'version': pin['version'], 'tag': pin['tag'],
            'pin_sha256': digest(pin_bytes), 'original_inputs': pin['files'],
            'cargo_lock': lock_proof,
            'recipe': windows_recipe(original)}


def package_identity(project, proof, package):
    allowed = {f"CrispSorter_{proof['version']}_x64-windows-portable.zip",
               f"CrispSorter_{proof['version']}_x64-windows-portable-unsigned.zip"}
    package = Path(package)
    if not package.is_absolute(): package = project / package
    if (package.name not in allowed or package.parent.resolve() != project.resolve()
            or package.is_symlink() or not package.is_file() or package.stat().st_size == 0):
        raise ValueError('Windows retry upload requires exactly the original-version portable archive in the app checkout')
    with package.open('rb') as stream:
        actual = hashlib.file_digest(stream, 'sha256').hexdigest()
    return {'filename': package.name, 'sha256': actual}


def seal(project, proof, receipt_path):
    expected_name = f"CrispSorter_{proof['version']}_windows-x86_64.receipt.json"
    if (receipt_path.name != expected_name or receipt_path.parent.resolve() != project.resolve()
            or receipt_path.is_symlink()):
        raise ValueError('Windows retry can seal only its exact Windows receipt in the app checkout')
    data = receipt_path.read_bytes()
    receipt = json.loads(data)
    expected_lock = json.loads(committed(project, 'scripts/crispembed-runtime-lock.json'))
    if (receipt.get('schema') != 'crispsorter-release-receipt-v1'
            or receipt.get('platform') != 'windows-x86_64'
            or receipt.get('app_source') != proof['app_source']
            or receipt.get('version') != proof['version']
            or receipt.get('runtime_lock') != expected_lock
            or receipt.get('dependency_sources', {}).get('CrispEmbed') != expected_lock['source_commit']):
        raise ValueError('Windows receipt does not match the original app/runtime source')
    artifacts = receipt.get('artifacts')
    if not isinstance(artifacts, list) or len(artifacts) != 1:
        raise ValueError('Windows retry receipt must identify exactly one portable archive')
    artifact = artifacts[0]
    if not isinstance(artifact, dict) or not isinstance(artifact.get('filename'), str):
        raise ValueError('Windows retry artifact entry is invalid')
    actual = package_identity(project, proof, Path(artifact.get('filename', '')))
    if actual['sha256'] != artifact.get('sha256'):
        raise ValueError('Windows retry portable archive changed after verification')
    checksums = project / f"CrispSorter_{proof['version']}_windows-x86_64.sha256"
    if checksums.is_symlink() or checksums.read_text() != f"{actual['sha256']}  {actual['filename']}\n":
        raise ValueError('Windows retry checksum file differs from the exact portable archive')
    receipt['workflow_source'] = proof['workflow_source']
    receipt['windows_original_retry'] = {**proof, 'verified_receipt_sha256': digest(data)}
    receipt_path.write_text(json.dumps(receipt, indent=2) + '\n')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--project', type=Path, default=Path.cwd())
    parser.add_argument('--repo', required=True)
    parser.add_argument('--phase', choices=('prepare', 'check', 'seal'), default='prepare')
    parser.add_argument('--output', type=Path)
    parser.add_argument('--receipt', type=Path)
    parser.add_argument('--package', type=Path)
    args = parser.parse_args()
    project = args.project.resolve()
    proof = verify(project, TOOLING, args.repo, os.environ, args.phase)
    if args.package:
        if args.phase != 'check': parser.error('--package is accepted only for the pre-upload check')
        proof['verified_upload'] = package_identity(project, proof, args.package)
    if args.phase == 'seal':
        if args.receipt is None: parser.error('--receipt is required for sealing')
        seal(project, proof, args.receipt)
    if args.output:
        args.output.write_text(json.dumps(proof, indent=2) + '\n')
    print(json.dumps(proof, indent=2))


if __name__ == '__main__':
    main()
