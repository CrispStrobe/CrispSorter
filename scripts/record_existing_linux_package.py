#!/usr/bin/env python3
"""Record bounded retained checks of one existing DEB; never a release receipt.

No build, inference, download, upload or license acceptance is performed here.
The distinct observation schema cannot satisfy the publication receipt gate.
"""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess

from check_release_target import validate_release_target

SOURCE = 'b739a3c0b70f39d3ae19e1bfcb412f3b44f77794'
VERSION = '0.15.1'
PACKAGE = 'CrispSorter_0.15.1_amd64.deb'
PACKAGE_BYTES = 229393246
PACKAGE_SHA256 = 'b1cc6d018bc1651c5a1604227b43cdc5b73019bad4ab627d94355f8e21cacd55'
MODELS = {'dbnet-ic15-q8_0.gguf': (12960448, '163c3b3d1211fd47b4ea323e2e684287bdc75ae68f0ffea397ccfcdc29a74de2'),
          'trocr-small-printed-q8_0.gguf': (67508320, '1aa4cdf3413539d7dbd53a565149abe4ca878f84a7ae2129e9a297f3a26b50d4')}
EVIDENCE_PIN = 'scripts/linux-existing-package-evidence-lock.json'


def sha(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def evidence_file(root, name, limit=2 * 1024 * 1024):
    path = root / name
    if path.is_symlink() or not path.is_file() or not 0 < path.stat().st_size <= limit:
        raise ValueError('Missing, symlinked or oversized retained evidence: ' + name)
    return path


def draft_identity(repo):
    if not re.fullmatch(r'[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+', repo):
        raise ValueError('Invalid repository name')
    validate_release_target(repo, 'v' + VERSION, SOURCE)
    pages = json.loads(subprocess.check_output(['gh', 'api', '--paginate', '--slurp',
                       f'repos/{repo}/releases?per_page=100'], text=True))
    if not isinstance(pages, list) or not all(isinstance(page, list) for page in pages):
        raise ValueError('Expected all release-list pages')
    matches = [row for page in pages for row in page if isinstance(row, dict) and row.get('tag_name') == 'v' + VERSION]
    if len(matches) != 1:
        raise ValueError('Expected exactly one original-source draft')
    draft = matches[0]
    if draft.get('draft') is not True or draft.get('target_commitish') != SOURCE:
        raise ValueError('Existing package observation requires the original unpublished draft target')
    assets = [row for row in draft.get('assets', []) if row.get('name') == PACKAGE]
    if (len(assets) != 1 or assets[0].get('size') != PACKAGE_BYTES
            or assets[0].get('digest') != 'sha256:' + PACKAGE_SHA256):
        raise ValueError('Current draft asset differs from the pinned existing DEB')
    return {'tag': 'v' + VERSION, 'target': SOURCE, 'unpublished': True,
            'asset_digest': 'sha256:' + PACKAGE_SHA256,
            'original_linux_release_receipt_present': any(row.get('name') == f'CrispSorter_{VERSION}_linux-x86_64.receipt.json' for row in draft['assets'])}


def record(root, tooling, draft):
    package = evidence_file(root, PACKAGE, PACKAGE_BYTES)
    if package.stat().st_size != PACKAGE_BYTES or sha(package) != PACKAGE_SHA256:
        raise ValueError('Retained DEB differs from the pinned existing package')
    git = lambda *args: subprocess.check_output(['git', '-C', str(tooling), *args], text=True).strip()
    if git('status', '--porcelain', '--untracked-files=no'):
        raise ValueError('Observation tooling must have clean committed sources')
    helper = 'scripts/record_existing_linux_package.py'
    if subprocess.check_output(['git', '-C', str(tooling), 'show', 'HEAD:' + helper]) != (tooling / helper).read_bytes():
        raise ValueError('Observation helper differs from its committed source')
    pin_bytes = subprocess.check_output(['git', '-C', str(tooling), 'show', 'HEAD:' + EVIDENCE_PIN])
    if (tooling / EVIDENCE_PIN).read_bytes() != pin_bytes:
        raise ValueError('Reviewed evidence pin differs from its committed source')
    pin = json.loads(pin_bytes)
    expected_files = {'partial-verification.json', 'version.json', 'doctor.json', 'worker-protocol.json',
                      'offline-control.json', 'offline-control.stdout.hocr', 'offline-control.stderr.txt', 'ocr-task.pdf'}
    if (pin.get('schema') != 'crispsorter-reviewed-linux-package-evidence-v1'
            or pin.get('original_app_source') != SOURCE
            or pin.get('package') != {'filename': PACKAGE, 'bytes': PACKAGE_BYTES, 'sha256': PACKAGE_SHA256}
            or not isinstance(pin.get('evidence_sha256'), dict) or set(pin['evidence_sha256']) != expected_files):
        raise ValueError('Reviewed evidence pin does not match the original source/package')
    # These committed digests are independent of all editable declarations in
    # the retained JSON. Replacing hOCR plus its declared digest cannot pass.
    for name, digest in pin['evidence_sha256'].items():
        path = evidence_file(root, name, 4 * 1024 * 1024)
        if not re.fullmatch('[0-9a-f]{64}', digest) or sha(path) != digest:
            raise ValueError('Retained evidence differs from the committed reviewed pin: ' + name)
    fixture = subprocess.check_output(['git', '-C', str(tooling), 'show', SOURCE + ':tests/quality/ocr-task.pdf'])
    fixture_sha = hashlib.sha256(fixture).hexdigest()
    if sha(evidence_file(root, 'ocr-task.pdf')) != fixture_sha:
        raise ValueError('Retained PDF fixture differs from the original application source')
    checks = {}
    for name in ['partial-verification.json', 'version.json', 'doctor.json', 'worker-protocol.json', 'offline-control.json']:
        checks[name] = json.loads(evidence_file(root, name).read_text())
    partial, version, worker, control = [checks[name] for name in ['partial-verification.json', 'version.json', 'worker-protocol.json', 'offline-control.json']]
    if (partial.get('schema') != 'crispsorter-local-linux-partial-verification-v1'
            or partial.get('app_source_from_completed_build_and_draft_target') != SOURCE
            or partial.get('version') != VERSION
            or partial.get('sha256') != PACKAGE_SHA256 or partial.get('bytes') != PACKAGE_BYTES
            or partial.get('matches_release_asset_digest') is not True
            or partial.get('cold_loader', {}).get('cache_inhibited') is not True
            or partial.get('cold_loader', {}).get('library_search_environment_removed') is not True
            or partial.get('cold_loader', {}).get('missing_libraries') != []
            or partial.get('package_checks') != {'version': 'passed', 'doctor': 'passed', 'ocr_worker_protocol': 'passed'}
            or version != {'name': 'crispsorter', 'version': VERSION, 'target': 'linux', 'arch': 'x86_64'}
            or not isinstance(checks['doctor.json'], dict)
            or checks['doctor.json'].get('pdf_extract_compiled_in') is not True
            or set(checks['doctor.json']) != {'tesseract_installed', 'ocrs_models_available', 'paddle_ocr_available',
                 'pdf_extract_compiled_in', 'fuse_compiled', 'fuse_runtime_available', 'embedder_model_cached', 'lance_dir_exists'}
            or any(type(value) is not bool for value in checks['doctor.json'].values())
            or worker.get('exit_codes') != [1, 1]
            or worker.get('arguments') != ['--crispsorter-ocr-worker']):
        raise ValueError('Retained package startup/protocol checks do not match the original package observation')
    if (control.get('schema') != 'crispsorter-offline-package-ocr-control-v1'
            or control.get('app_source_from_completed_build') != SOURCE or control.get('package_sha256') != PACKAGE_SHA256
            or control.get('fixture_sha256') != fixture_sha or control.get('status') != 'passed'
            or control.get('exit_code') != 0 or control.get('stopped') is not None
            or control.get('engine') != 'dbnet_trocr' or control.get('offline') is not True
            or control.get('license_acceptance_added') is not False
            or control.get('gpu_device_nodes_present') is not False
            or control.get('model_bytes_unchanged_after_control') is not True
            or control.get('qwen_preset_smoke') != 'blocked_license_consent_required; not attempted'):
        raise ValueError('Retained offline inference does not match the original package/fixture control')
    models = control.get('models', [])
    if (len(models) != 2 or {row.get('filename') for row in models} != set(MODELS)
            or any((row.get('bytes'), row.get('sha256')) != MODELS[row['filename']] for row in models)):
        raise ValueError('Retained inference model identities differ from the verified inputs')
    for name in ['stdout.hocr', 'stderr.txt']:
        path = evidence_file(root, 'offline-control.' + name, 4 * 1024 * 1024)
        if sha(path) != control.get('sanitized_output_sha256', {}).get(name):
            raise ValueError('Retained inference output changed: ' + name)
        checks['offline-control.' + name] = {'sha256': sha(path)}
    text = (root / 'offline-control.stdout.hocr').read_text().lower()
    if not all(word in text for word in ['ocr_page', 'cobalt', 'warranty', '2031']):
        raise ValueError('Retained packaged inference missed required fixture evidence')
    return {'schema': 'crispsorter-existing-linux-package-observation-v1',
            'original_app_source_association': SOURCE, 'version': VERSION,
            'verification_tooling_source': git('rev-parse', 'HEAD'),
            'verification_tooling_tree': git('rev-parse', 'HEAD^{tree}'),
            'reviewed_evidence_pin_sha256': hashlib.sha256(pin_bytes).hexdigest(),
            'draft_observation': draft,
            'package': {'filename': PACKAGE, 'bytes': PACKAGE_BYTES, 'sha256': PACKAGE_SHA256},
            'retained_evidence_sha256': {name: sha(root / name) for name in [*checks, 'ocr-task.pdf']},
            'permissive_package_control': 'passed; retained observations only, not rerun by this recorder',
            'licensed_qwen_presets': 'unqualified; original smoke blocked on license consent',
            'grants_license_consent': False, 'satisfies_publication_gate': False,
            'source_binding_limit': 'Original build log/draft target source association only; final dependency-lock bytes were not retained. This is not an original release receipt or a reconstructed compiled-source attestation.'}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repo', required=True)
    parser.add_argument('--evidence-dir', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    if args.output.name != f'CrispSorter_{VERSION}_linux-x86_64.observation.json':
        parser.error('Use the separate .observation.json filename, never an original release/OCR receipt name')
    result = record(args.evidence_dir.resolve(), Path(__file__).resolve().parents[1], draft_identity(args.repo))
    with args.output.open('x') as stream:
        stream.write(json.dumps(result, indent=2) + '\n')


if __name__ == '__main__':
    main()
