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
                'platform': platform, 'version': '0.13', 'app_source': 'source', 'artifacts': artifacts}))
            self.names.append(receipt_name)

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
                '--repo', 'example/repo', '--source', 'source', '--version', '0.13']):
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


if __name__ == '__main__':
    unittest.main()
