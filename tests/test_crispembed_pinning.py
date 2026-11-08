#!/usr/bin/env python3
"""CrispEmbed pin parity: every workflow and helper follows the runtime lock.

`scripts/crispembed-runtime-lock.json` is the single source of truth for the
CrispEmbed release (tag, source commit, archive digests). Workflows check out
the exact source commit, never a moving tag or `releases/latest`, and verify
downloaded archives with `scripts/verify_crispembed_bundle.py`, which reads
the same lock (its own behaviour is covered by `scripts/test_runtime_lock.py`).
"""
import json
import pathlib
import re
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[1]
LOCK = json.loads((ROOT / 'scripts/crispembed-runtime-lock.json').read_text(encoding='utf-8'))
TAG = 'v' + LOCK['version']
COMMIT = LOCK['source_commit']
WORKFLOWS = ['.github/workflows/ci.yml', '.github/workflows/release.yml']


def read(path):
    return (ROOT / path).read_text(encoding='utf-8')


class CrispEmbedPinning(unittest.TestCase):
    def test_lock_is_well_formed(self):
        self.assertRegex(COMMIT, r'^[0-9a-f]{40}$')
        self.assertTrue(LOCK['assets'])
        for name, asset in LOCK['assets'].items():
            self.assertRegex(asset['sha256'], r'^[0-9a-f]{64}$', name)

    def test_workflows_pin_the_locked_tag_and_source_commit(self):
        for path in WORKFLOWS:
            with self.subTest(path=path):
                s = read(path)
                self.assertRegex(s, rf'(?m)^  CRISPEMBED_REF: {re.escape(TAG)}\b')
                self.assertRegex(s, rf'(?m)^  CRISPEMBED_COMMIT: {COMMIT}\b')
                checkouts = re.findall(
                    r'repository: \$\{\{ env\.CRISPEMBED_REPO \}\}\n\s+ref: ([^\n]+)', s)
                self.assertTrue(checkouts)
                self.assertTrue(all(x.strip() == '${{ env.CRISPEMBED_COMMIT }}' for x in checkouts), checkouts)
                self.assertNotRegex(s, r'CrispEmbed/releases/latest')
                self.assertNotIn('crispembed_sha256', s)

    def test_downloaded_archives_are_verified_against_the_lock(self):
        for path in WORKFLOWS:
            with self.subTest(path=path):
                s = read(path)
                downloads = s.count('crispembed-libs.')
                self.assertGreater(downloads, 0)
                self.assertIn('scripts/verify_crispembed_bundle.py', s)

    def test_helpers_read_the_lock(self):
        for path in ['enable-crispembed.sh', 'enable-crispembed.ps1']:
            with self.subTest(path=path):
                self.assertIn('crispembed-runtime-lock.json', read(path))


if __name__ == '__main__':
    unittest.main()
