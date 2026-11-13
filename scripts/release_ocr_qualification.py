#!/usr/bin/env python3
"""Bind the separately consent-gated native preset smoke to a verified package.

This record grants no license consent. The unchanged native test must finish
successfully before sealing; package verification itself uses DBNet/TrOCR only.
"""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess

SCHEMA = 'crispsorter-linux-native-ocr-qualification-v1'
PRESETS = {'quick', 'multilingual', 'tables', 'difficult'}
MODELS = {'dbnet-det', 'trocr-printed', 'qwen2vl-3b'}


def sha(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def fixture_sha256(source, root):
    """Use committed source bytes, never an editable digest in a raw report."""
    data = subprocess.check_output(['git', '-C', str(root), 'show', source + ':tests/quality/ocr-task.png'])
    return hashlib.sha256(data).hexdigest()


def package_control():
    return {'permissive_package_control': {'status': 'passed', 'engine': 'dbnet_trocr',
                                         'models': ['dbnet-det', 'trocr-printed']},
            'licensed_native_presets': {'status': 'not_run', 'model': 'qwen2vl-3b',
                                       'qualification': 'not established',
                                       'required_before_publication': True,
                                       'reason': 'Separate license-gated native test; package verification grants no consent'}}


def package_entry(receipt, source, version):
    if not isinstance(receipt, dict) or not re.fullmatch(r'[0-9a-f]{40}', source):
        raise ValueError('Linux qualification requires an exact source commit and package receipt')
    artifacts = receipt.get('artifacts')
    if (receipt.get('schema') != 'crispsorter-release-receipt-v1'
            or receipt.get('platform') != 'linux-x86_64'
            or receipt.get('app_source') != source or receipt.get('version') != version
            or receipt.get('ocr_qualification') != package_control()
            or not isinstance(artifacts, list) or len(artifacts) != 1):
        raise ValueError('Linux package receipt does not match this source/version and completed permissive control')
    artifact = artifacts[0]
    if (not isinstance(artifact, dict)
            or artifact.get('filename') != f'CrispSorter_{version}_amd64.deb'
            or not re.fullmatch(r'[0-9a-f]{64}', artifact.get('sha256', ''))
            or type(artifact.get('bytes')) is not int or artifact['bytes'] <= 0):
        raise ValueError('Linux package receipt has an invalid artifact identity')
    return {key: artifact[key] for key in ['filename', 'bytes', 'sha256']}


def validate_native_smoke(smoke):
    if not isinstance(smoke, dict):
        raise ValueError('Native preset report must be an object')
    models, cases = smoke.get('models'), smoke.get('cases')
    if (not isinstance(models, list) or len(models) != len(MODELS)
            or any(not isinstance(row, dict) or not isinstance(row.get('registry_name'), str) for row in models)
            or {row.get('registry_name') for row in models} != MODELS
            or any(type(row.get('bytes')) is not int or row['bytes'] <= 0
                   or not re.fullmatch(r'[0-9a-f]{64}', row.get('sha256', '')) for row in models)):
        raise ValueError('Native preset report lacks the exact inspected model identities')
    if (not isinstance(cases, list) or len(cases) != len(PRESETS)
            or any(not isinstance(row, dict) or not isinstance(row.get('preset'), str) for row in cases)
            or {row.get('preset') for row in cases} != PRESETS
            or any(row.get('matched') is not True or not isinstance(row.get('text'), str)
                   or not all(word in row['text'].lower() for word in ['cobalt', 'warranty', '2031'])
                   for row in cases)):
        raise ValueError('Native preset report does not qualify every required preset')


def seal(receipt, receipt_sha256, smoke, source, version, expected_fixture_sha256):
    if (not re.fullmatch(r'[0-9a-f]{64}', receipt_sha256)
            or not re.fullmatch(r'[0-9a-f]{64}', expected_fixture_sha256)):
        raise ValueError('Package receipt and committed fixture require exact SHA-256 identities')
    package = package_entry(receipt, source, version)
    validate_native_smoke(smoke)
    if (smoke.get('schema') != 'crispsorter-native-ocr-presets-v1'
            or smoke.get('app_source') != source
            or smoke.get('fixture') != {'filename': 'ocr-task.png', 'sha256': expected_fixture_sha256}
            or smoke.get('package_receipt') != {'sha256': receipt_sha256, 'package': package}):
        raise ValueError('Raw native evidence belongs to another source, fixture or package receipt')
    return {'schema': SCHEMA, 'app_source': source, 'version': version,
            'package': package, 'package_receipt_sha256': receipt_sha256,
            'status': 'passed', 'scope': 'Synthetic native app-binding presets; package DBNet/TrOCR has its separate control',
            'license_consent': 'Enforced by the unchanged app model resolver; this record grants no consent',
            'native_smoke': smoke}


def validate(report, receipt, receipt_sha256, source, version, expected_fixture_sha256):
    if not isinstance(report, dict):
        raise ValueError('Licensed native OCR qualification must be an object')
    expected = seal(receipt, receipt_sha256, report.get('native_smoke', {}), source, version, expected_fixture_sha256)
    if report != expected:
        raise ValueError('Licensed native OCR qualification does not match the exact source/package receipt')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--receipt', required=True, type=Path)
    parser.add_argument('--native-smoke', required=True, type=Path)
    parser.add_argument('--output', required=True, type=Path)
    args = parser.parse_args()
    root = Path.cwd()
    source = subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip()
    version = json.loads((root / 'src-tauri/tauri.conf.json').read_text())['version']
    receipt = json.loads(args.receipt.read_text())
    identity = package_entry(receipt, source, version)
    packages = list(root.glob('target/**/bundle/deb/' + identity['filename']))
    if len(packages) != 1 or packages[0].is_symlink() or not packages[0].is_file():
        raise ValueError('Expected exactly one built Linux package for qualification')
    if packages[0].stat().st_size != identity['bytes'] or sha(packages[0]) != identity['sha256']:
        raise ValueError('Linux package changed after package verification')
    report = seal(receipt, sha(args.receipt), json.loads(args.native_smoke.read_text()), source, version,
                  fixture_sha256(source, root))
    with args.output.open('x') as stream:
        stream.write(json.dumps(report, indent=2) + '\n')


if __name__ == '__main__':
    main()
