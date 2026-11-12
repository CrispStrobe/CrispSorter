#!/usr/bin/env python3
import hashlib
from pathlib import Path
import plistlib
import tempfile
import unittest
import zipfile

from check_testflight_ipa import check_ipa


class IpaIdentity(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.ipa = Path(self.directory.name) / 'application.ipa'

    def write_ipa(self, version='0.15.1', build='123', bundle='com.crispstrobe.crispsorter', profile=True, executable=True, extra_app=False):
        with zipfile.ZipFile(self.ipa, 'w') as archive:
            archive.writestr('Payload/CrispSorter.app/Info.plist', plistlib.dumps({
                'CFBundleIdentifier': bundle, 'CFBundleShortVersionString': version,
                'CFBundleVersion': build, 'CFBundleExecutable': 'CrispSorter'}))
            if executable:
                archive.writestr('Payload/CrispSorter.app/CrispSorter', b'executable fixture; no signature claim')
            if profile:
                archive.writestr('Payload/CrispSorter.app/embedded.mobileprovision', b'profile fixture')
            if extra_app:
                archive.writestr('Payload/Another.app/Info.plist', b'extra')

    def test_current_and_retained_version_identity_remain_available(self):
        for version in ['0.15.0', '0.15.1']:
            self.write_ipa(version=version)
            digest = hashlib.sha256(self.ipa.read_bytes()).hexdigest()
            result = check_ipa(self.ipa, version, digest)
            self.assertEqual(result['version'], version)
            self.assertEqual(result['build_number'], '123')
            self.assertEqual(result['sha256'], digest)

    def test_source_version_or_bundle_mismatch_is_rejected(self):
        self.write_ipa(version='0.15.0')
        with self.assertRaisesRegex(ValueError, 'identity/version'):
            check_ipa(self.ipa, '0.15.1')
        self.write_ipa(bundle='another.application')
        with self.assertRaisesRegex(ValueError, 'identity/version'):
            check_ipa(self.ipa, '0.15.1')

    def test_wrong_hash_or_mutated_bytes_are_rejected(self):
        self.write_ipa()
        digest = hashlib.sha256(self.ipa.read_bytes()).hexdigest()
        with self.assertRaisesRegex(ValueError, 'SHA-256'):
            check_ipa(self.ipa, '0.15.1', '0' * 64)
        self.write_ipa(build='124')
        with self.assertRaisesRegex(ValueError, 'SHA-256'):
            check_ipa(self.ipa, '0.15.1', digest)

    def test_unsigned_fallback_or_multiple_applications_do_not_qualify(self):
        for change in [{'profile': False}, {'executable': False}, {'extra_app': True}, {'build': ''}, {'build': 'latest'}]:
            with self.subTest(change=change):
                self.write_ipa(**change)
                with self.assertRaises((ValueError, KeyError)):
                    check_ipa(self.ipa, '0.15.1')


if __name__ == '__main__':
    unittest.main()
