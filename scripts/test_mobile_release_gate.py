#!/usr/bin/env python3
"""Exercise publication against missing, stale, and changed draft mobile assets."""
import hashlib
import json
from pathlib import Path
import shutil
import sys
import tempfile
import unittest
from unittest.mock import patch

import check_mobile_release_assets as gate


class MobileGate(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.names = set()
        self.draft = True
        self.receipts = {}
        for platform, filename in {
            'android-arm64': 'CrispSorter_0.15.0_android-aarch64.apk',
            'ios-arm64': 'CrispSorter_0.15.0_ios-arm64.ipa',
        }.items():
            content = filename.encode()
            digest = hashlib.sha256(content).hexdigest()
            self.write(filename, content)
            item = {'filename': filename, 'sha256': digest}
            if platform == 'ios-arm64':
                item.update(embedded_profile=True, version='0.15.0', bundle_id='com.crispstrobe.crispsorter')
            else:
                item['signature_verification'] = 'verified APK signature'
                self.signing = filename.removesuffix('.apk') + '.signing.json'
                self.write(self.signing, {'schema': 'crispsorter-apk-signing-v1', 'filename': filename,
                    'sha256': digest, 'signing': 'provided production certificate', 'verification': 'verified'})
            receipt_name = f'CrispSorter_0.15.0_{platform}.receipt.json'
            self.receipts[platform] = receipt_name
            self.write(receipt_name, {'schema': 'crispsorter-release-receipt-v1', 'platform': platform,
                'version': '0.15.0', 'app_source': 'current-source', 'artifacts': [item]})

    def write(self, name, value):
        self.names.add(name)
        (self.root / name).write_bytes(value if isinstance(value, bytes) else json.dumps(value).encode())

    def change_json(self, name, mutate):
        value = json.loads((self.root / name).read_text())
        mutate(value)
        self.write(name, value)

    def fake_gh(self, *args):
        if args[:2] == ('release', 'view'):
            return json.dumps({'isDraft': self.draft, 'assets': [{'name': n} for n in self.names]})
        if args[:2] == ('release', 'download'):
            name = args[args.index('--pattern') + 1]
            shutil.copy(self.root / name, Path(args[args.index('--dir') + 1]) / name)
            return ''
        raise AssertionError(args)

    def run_gate(self):
        with patch.object(gate, 'gh', self.fake_gh), patch.object(sys, 'argv', [
                'gate', '--tag', 'v0.15.0', '--repo', 'example/repo', '--source', 'current-source', '--version', '0.15.0']):
            gate.main()

    def test_complete_current_packages_pass(self):
        self.run_gate()

    def test_unsigned_ios_fallback_cannot_publish(self):
        self.names.remove('CrispSorter_0.15.0_ios-arm64.ipa')
        self.names.add('CrispSorter_0.15.0_ios-arm64-unsigned.app.tar.gz')
        with self.assertRaisesRegex(ValueError, 'Missing or stale'):
            self.run_gate()

    def test_missing_receipt_cannot_publish(self):
        self.names.remove(self.receipts['ios-arm64'])
        with self.assertRaisesRegex(ValueError, 'Missing mobile release asset'):
            self.run_gate()

    def test_stale_source_receipt_cannot_publish(self):
        self.change_json(self.receipts['android-arm64'], lambda r: r.update(app_source='older-source'))
        with self.assertRaisesRegex(ValueError, 'source/version'):
            self.run_gate()

    def test_overwritten_package_cannot_publish(self):
        self.write('CrispSorter_0.15.0_ios-arm64.ipa', b'changed after inspection')
        with self.assertRaisesRegex(ValueError, 'does not match'):
            self.run_gate()

    def test_stale_additional_package_cannot_publish(self):
        self.names.add('CrispSorter_0.14.0_android-aarch64.apk')
        with self.assertRaisesRegex(ValueError, 'Missing or stale'):
            self.run_gate()

    def test_development_signing_cannot_publish(self):
        self.change_json(self.signing, lambda r: r.update(signing='development certificate; not a production/store identity'))
        with self.assertRaisesRegex(ValueError, 'production signing'):
            self.run_gate()

    def test_signing_receipt_for_different_bytes_cannot_publish(self):
        self.change_json(self.signing, lambda r: r.update(sha256='0' * 64))
        with self.assertRaisesRegex(ValueError, 'production signing'):
            self.run_gate()

    def test_unprovisioned_ios_cannot_publish(self):
        self.change_json(self.receipts['ios-arm64'], lambda r: r['artifacts'][0].update(embedded_profile=False))
        with self.assertRaisesRegex(ValueError, 'provisioned'):
            self.run_gate()

    def test_public_release_rejected(self):
        self.draft = False
        with self.assertRaisesRegex(ValueError, 'already public'):
            self.run_gate()

    def test_gate_precedes_publication(self):
        workflow = (Path(__file__).resolve().parents[1] / '.github/workflows/release.yml').read_text()
        publish = workflow.split('\n  publish:', 1)[1]
        self.assertLess(publish.index('python3 scripts/check_mobile_release_assets.py'),
                        publish.index('gh release edit'))
        self.assertIn('set -euo pipefail', publish)


if __name__ == '__main__':
    unittest.main()
