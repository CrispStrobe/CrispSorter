#!/usr/bin/env python3
"""Ordinary releases remain usable; requesting unintegrated OCR fails early."""
import os
import json
import sys
import tempfile

import verify_release_artifacts as verifier
from pathlib import Path
import unittest
from unittest.mock import patch

from check_mobile_release_profile import profile


class MobileProfile(unittest.TestCase):
    def test_standard_release_discloses_native_ocr_and_qualification_limits(self):
        result = profile('standard')
        self.assertEqual(result['recipe'], 'standard')
        self.assertEqual(result['native_ocr'], 'not included by this release recipe')
        self.assertEqual(result['ocr_qualification'], 'not established')

    def test_ocr_request_is_not_satisfied_by_baseline_recipe(self):
        with self.assertRaisesRegex(ValueError, 'not implemented'):
            profile('ocr')

    def test_environment_ocr_request_fails_closed(self):
        with patch.dict(os.environ, {'CRISPSORTER_MOBILE_RELEASE_PROFILE': 'ocr'}):
            with self.assertRaisesRegex(ValueError, 'not implemented'):
                profile()

    def test_default_preserves_ordinary_release_route(self):
        with patch.dict(os.environ, {}, clear=True):
            self.assertEqual(profile()['requested'], 'standard')

    def test_unknown_profile_rejected(self):
        with self.assertRaisesRegex(ValueError, 'Unknown'):
            profile('candidate')

    def test_optional_mobile_diagnostics_disclose_profile_without_source_admission(self):
        for platform, suffix in [('android-arm64', 'android-aarch64.apk'), ('ios-arm64', 'ios-arm64.ipa')]:
            with self.subTest(platform=platform), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                (root / 'src-tauri').mkdir()
                (root / 'scripts').mkdir()
                (root / 'src-tauri/tauri.conf.json').write_text('{"version":"0.15.0"}')
                (root / 'scripts/crispembed-runtime-lock.json').write_text('{"source_commit":"production-runtime"}')
                (root / ('CrispSorter_0.15.0_' + suffix)).write_bytes(b'package fixture')
                with patch.object(verifier.Path, 'cwd', return_value=root), \
                        patch.object(verifier.subprocess, 'check_output', return_value='source-or-tool-version'), \
                        patch.object(verifier, 'inspect_package', return_value={'inspection': 'fixture'}), \
                        patch.object(sys, 'argv', ['verify', '--platform', platform]), \
                        patch.dict(os.environ, {'CRISPSORTER_MOBILE_RELEASE_PROFILE': 'standard'}):
                    verifier.main()
                receipt = json.loads((root / f'CrispSorter_0.15.0_{platform}.receipt.json').read_text())
                self.assertNotIn('app_source',receipt)
                self.assertFalse(receipt['native_crispembed_included'])
                self.assertEqual(receipt['mobile_release_profile']['ocr_qualification'], 'not established')
                self.assertEqual(receipt['mobile_release_profile']['native_ocr'], 'not included by this release recipe')

    def test_workflow_checks_before_build_or_retained_ipa_download(self):
        root = Path(__file__).resolve().parents[1]
        workflow = (root / '.github/workflows/release.yml').read_text()
        preflight = workflow.split('\n  release-draft-preflight:', 1)[1].split('\n  release-required:', 1)[0]
        self.assertLess(preflight.index('check_mobile_release_profile.py'), preflight.index('gh release'))
        retained = workflow.split('\n  testflight-existing:', 1)[1].split('\n  release-draft-preflight:', 1)[0]
        self.assertLess(retained.index('check_mobile_release_profile.py'), retained.index('gh release download'))
        self.assertIn("env.TESTFLIGHT_UPLOAD_REQUIRED == 'true'", retained)
        self.assertNotIn('Set the $IPA_VERSION upload timestamp before resuming.', retained)
        verifier = (root / 'scripts/verify_release_artifacts.py').read_text()
        self.assertIn("receipt['mobile_release_profile'] = profile()", verifier)


if __name__ == '__main__':
    unittest.main()
