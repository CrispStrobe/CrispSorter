#!/usr/bin/env python3
"""Keep fresh-upload TestFlight selectors bound to the uploaded artifact."""
from pathlib import Path
import unittest
from unittest.mock import patch
from urllib.parse import parse_qs, urlsplit

import testflight_distribute as distribution


class BuildSelection(unittest.TestCase):
    def select(self, platform, builds):
        class FakeASC:
            def get(self, path):
                query = parse_qs(urlsplit(path).query)
                self.query = query
                return {'data': builds}, 200
        asc = FakeASC()
        with patch.object(distribution.time, 'sleep', side_effect=AssertionError('unexpected wait')):
            result = distribution.latest_build(
                asc, 'app', platform, version='42', wait_minutes=-1,
                uploaded_after='2026-11-12T12:00:00Z')
        self.assertEqual(asc.query['filter[version]'], ['42'])
        self.assertEqual(asc.query['filter[preReleaseVersion.platform]'], [platform])
        return result

    @staticmethod
    def build(identifier, version, uploaded):
        return {'id': identifier, 'attributes': {
            'version': version, 'uploadedDate': uploaded, 'processingState': 'VALID'}}

    def test_selects_exact_fresh_build_on_both_platforms(self):
        for platform in ['IOS', 'MAC_OS']:
            with self.subTest(platform=platform):
                builds = [self.build('other-build', '43', '2026-11-12T12:02:00Z'),
                          self.build('old-same-number', '42', '2026-11-11T12:00:00Z'),
                          self.build('fresh', '42', '2026-11-12T12:00:00Z')]
                self.assertEqual(self.select(platform, builds)['id'], 'fresh')

    def test_refuses_prior_or_unrelated_valid_build(self):
        for platform in ['IOS', 'MAC_OS']:
            for build in [self.build('old', '42', '2026-11-11T12:00:00Z'),
                          self.build('wrong-number', '43', '2026-11-12T12:02:00Z'),
                          self.build('missing-time', '42', None)]:
                with self.subTest(platform=platform, build=build['id']):
                    with self.assertRaisesRegex(SystemExit, 'no VALID build'):
                        self.select(platform, [build])


class WorkflowBinding(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.workflow = (Path(__file__).resolve().parents[1] / '.github/workflows/release.yml').read_text()

    def job(self, name, next_name):
        return self.workflow.split(f'\n  {name}:', 1)[1].split(f'\n  {next_name}:', 1)[0]

    def test_ios_identity_comes_from_uploaded_ipa_and_cutoff_precedes_upload(self):
        job = self.job('release-ios', 'release-macos-appstore')
        self.assertIn('Print CFBundleVersion\' "$RUNNER_TEMP/ipaplist/$APP/Info.plist"', job)
        self.assertLess(job.index('BV=$('), job.index('echo "IOS_BUILD_NUMBER=$BV"'))
        self.assertLess(job.index('echo "IOS_UPLOADED_AFTER=$(date -u'),
                        job.index('xcrun altool --upload-package "$IPA"'))
        self.assertIn('--version "$IOS_BUILD_NUMBER" --uploaded-after "$IOS_UPLOADED_AFTER"', job)
        self.assertIn("if: env.CRISPSORTER_MOBILE_RELEASE_PROFILE != 'ocr-draft' && env.IPA_UPLOADED == 'true'", job)

    def test_macos_cutoff_precedes_upload_and_distribution_requires_success(self):
        job = self.job('release-macos-appstore', 'publish')
        self.assertLess(job.index('echo "MACOS_UPLOADED_AFTER=$(date -u'),
                        job.index('xcrun altool --upload-package "$PKG"'))
        self.assertIn('--version "$BV" --uploaded-after "$MACOS_UPLOADED_AFTER"', job)
        self.assertIn("if: env.UPLOADED == 'true'", job)

    def test_resume_still_requires_explicit_original_upload_timestamp(self):
        job = self.job('testflight-existing', 'release-draft-preflight')
        self.assertIn("if: inputs.platform == 'testflight_resume'", job)
        self.assertIn('Set the $IPA_VERSION upload timestamp before resuming.', job)
        self.assertIn('--version "$BUILD_NUMBER" --uploaded-after "$UPLOADED_AFTER"', job)


if __name__ == '__main__':
    unittest.main()
