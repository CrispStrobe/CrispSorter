#!/usr/bin/env python3
"""Check publication gating against simulated draft-release API responses."""
import hashlib
import json
from pathlib import Path
import shutil
import sys
import tempfile
import unittest
from unittest.mock import patch
import check_desktop_release_assets as gate
import release_ocr_qualification as qualification

SOURCE = '1' * 40


class DesktopGate(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.names = []
        self.draft = True
        for platform, files in {
            'macos-arm64': ['CrispSorter_0.13_aarch64.dmg', 'CrispSorter_aarch64.app.tar.gz'],
            'windows-x86_64': ['CrispSorter_0.13_x64-windows-portable.zip'],
        }.items():
            artifacts = []
            for name in files:
                content = ('content ' + name).encode()
                (self.root / name).write_bytes(content)
                artifacts.append({'filename': name, 'sha256': hashlib.sha256(content).hexdigest(),
                                  'signature_verification': 'verified on target host'})
                self.names.append(name)
            receipt_name = f'CrispSorter_0.13_{platform}.receipt.json'
            (self.root / receipt_name).write_text(json.dumps({'schema': 'crispsorter-release-receipt-v1',
                'platform': platform, 'version': '0.13', 'app_source': SOURCE, 'artifacts': artifacts}))
            self.names.append(receipt_name)
        package = 'CrispSorter_0.13_amd64.deb'
        content = b'nonexecutable Linux package fixture'
        (self.root / package).write_bytes(content)
        self.names.append(package)
        receipt = {'schema': 'crispsorter-release-receipt-v1', 'platform': 'linux-x86_64',
                   'version': '0.13', 'app_source': SOURCE, 'ocr_qualification': qualification.package_control(),
                   'artifacts': [{'filename': package, 'bytes': len(content), 'sha256': hashlib.sha256(content).hexdigest()}]}
        receipt_name = 'CrispSorter_0.13_linux-x86_64.receipt.json'
        (self.root / receipt_name).write_text(json.dumps(receipt))
        self.names.append(receipt_name)
        native = {'models': [{'registry_name': name, 'bytes': 1, 'sha256': 'a' * 64} for name in qualification.MODELS],
                  'cases': [{'preset': name, 'text': 'Cobalt warranty 2031', 'matched': True} for name in qualification.PRESETS]}
        report = qualification.seal(receipt, gate.sha(self.root / receipt_name), native, SOURCE, '0.13')
        ocr_name = 'CrispSorter_0.13_linux-x86_64.ocr.json'
        (self.root / ocr_name).write_text(json.dumps(report))
        self.names.append(ocr_name)

    def fake_gh(self, *args):
        if args[:2] == ('release', 'view'):
            return json.dumps({'isDraft': self.draft, 'assets': [{'name': n} for n in self.names]})
        if args[:2] == ('release', 'download'):
            name = args[args.index('--pattern') + 1]
            directory = Path(args[args.index('--dir') + 1])
            shutil.copy(self.root / name, directory / name)
            return ''
        raise AssertionError(args)

    def run_gate(self):
        with patch.object(gate, 'gh', self.fake_gh), patch.object(sys, 'argv', ['gate', '--tag', 'v0.13',
                '--repo', 'example/repo', '--source', SOURCE, '--version', '0.13']):
            gate.main()

    def test_matches_verified_assets(self):
        self.run_gate()

    def test_rejects_modified_asset(self):
        (self.root / 'CrispSorter_0.13_aarch64.dmg').write_bytes(b'changed')
        with self.assertRaisesRegex(ValueError, 'does not match'):
            self.run_gate()

    def test_rejects_stale_unsigned_download(self):
        self.names.append('CrispSorter_0.13_aarch64-unsigned.dmg')
        with self.assertRaisesRegex(ValueError, 'stale'):
            self.run_gate()

    def test_rejects_public_release(self):
        self.draft = False
        with self.assertRaisesRegex(ValueError, 'already public'):
            self.run_gate()

    def test_draft_package_without_qwen_qualification_cannot_publish(self):
        self.names.remove('CrispSorter_0.13_linux-x86_64.ocr.json')
        with self.assertRaisesRegex(ValueError, 'Missing Linux package/licensed'):
            self.run_gate()

    def test_modified_linux_package_cannot_publish(self):
        (self.root / 'CrispSorter_0.13_amd64.deb').write_bytes(b'changed')
        with self.assertRaisesRegex(ValueError, 'Linux package does not match'):
            self.run_gate()

    def test_linux_qualification_rejects_stale_source_and_false_success(self):
        path = self.root / 'CrispSorter_0.13_linux-x86_64.ocr.json'
        original = json.loads(path.read_text())
        for case in ['source', 'skipped', 'failed-preset', 'receipt-hash']:
            value = json.loads(json.dumps(original))
            if case == 'source': value['app_source'] = '2' * 40
            elif case == 'skipped': value['status'] = 'not_run'
            elif case == 'failed-preset': value['native_smoke']['cases'][0]['matched'] = False
            else: value['package_receipt_sha256'] = 'b' * 64
            path.write_text(json.dumps(value))
            with self.subTest(case=case), self.assertRaises(ValueError):
                self.run_gate()


if __name__ == '__main__':
    unittest.main()
