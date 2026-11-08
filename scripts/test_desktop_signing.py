#!/usr/bin/env python3
"""Exercise macOS signing control flow with mocks; this does not verify real signatures."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().with_name('sign_macos_release.sh')
FINGERPRINT = '0123456789ABCDEF0123456789ABCDEF01234567'


class MacSigningControlFlow(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.bin = self.root / 'bin'
        self.bin.mkdir()
        self.app = self.root / 'CrispSorter.app'
        (self.app / 'Contents/MacOS').mkdir(parents=True)
        (self.app / 'Contents/MacOS/crispsorter').touch()
        self.key = self.root / 'notary.p8'
        self.key.touch()
        self.dmg = self.root / 'release.dmg'
        self.env = dict(os.environ, PATH=str(self.bin) + os.pathsep + os.environ['PATH'],
                        DEVELOPER_ID_SHA1=FINGERPRINT, APPLE_TEAM_ID='N9XSJ4M3GT',
                        APPLE_NOTARY_KEY_PATH=str(self.key), APPLE_NOTARY_KEY_ID='KEY',
                        APPLE_NOTARY_ISSUER_ID='ISSUER', MOCK_NOTARY_STATUS='Accepted')
        mocks = {
            'security': f'echo "  1) {FINGERPRINT} \\\"Developer ID Application: Example (N9XSJ4M3GT)\\\""',
            'file': 'echo "Mach-O 64-bit executable"',
            'codesign': '''if [ "$1" = "-dv" ]; then
  printf 'Authority=Developer ID Application: Example (N9XSJ4M3GT)\\nTeamIdentifier=N9XSJ4M3GT\\nTimestamp=now\\nflags=runtime\\n' >&2
fi''',
            'ditto': 'touch "$5"',
            'spctl': ':',
            'hdiutil': 'for arg in "$@"; do :; done; touch "$arg"',
            'xcrun': '''if [ "$1" = notarytool ] && [ "$2" = submit ]; then
  printf '{"id":"example-id","status":"%s"}\\n' "$MOCK_NOTARY_STATUS"
fi''',
        }
        for name, body in mocks.items():
            path = self.bin / name
            path.write_text('#!/bin/sh\nset -e\n' + body + '\n')
            path.chmod(0o755)

    def run_script(self):
        return subprocess.run([str(SCRIPT), str(self.app), str(self.dmg)],
                              env=self.env, capture_output=True, text=True)

    def test_requires_notary_credentials(self):
        del self.env['APPLE_NOTARY_KEY_ID']
        result = self.run_script()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('APPLE_NOTARY_KEY_ID', result.stderr)
        self.assertFalse(self.dmg.exists())

    def test_rejects_distribution_identity(self):
        path = self.bin / 'security'
        path.write_text(f'#!/bin/sh\necho "{FINGERPRINT} Apple Distribution: Example (N9XSJ4M3GT)"\n')
        path.chmod(0o755)
        result = self.run_script()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('Developer ID Application identity', result.stderr)
        self.assertFalse(self.dmg.exists())

    def test_rejects_nonaccepted_notarization(self):
        self.env['MOCK_NOTARY_STATUS'] = 'Invalid'
        result = self.run_script()
        self.assertNotEqual(result.returncode, 0, result.stderr)
        self.assertIn('Notarization failed', result.stderr)
        self.assertFalse(self.dmg.exists())

    def test_accepted_mock_flow_creates_dmg(self):
        result = self.run_script()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(self.dmg.is_file())
        self.assertEqual(result.stdout.count('Notarization accepted:'), 2)


if __name__ == '__main__':
    unittest.main()
