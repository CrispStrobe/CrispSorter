"""Actual APK/IPA release downloads, without source or receipt prerequisites."""
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch
import check_mobile_release_assets as gate
import release_package_assets as assets
import test_release_package_assets as fixtures


class MobilePackages(unittest.TestCase):
    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory();self.addCleanup(self.tmp.cleanup);self.root=Path(self.tmp.name)
        fixtures.apk(self.root/f'CrispSorter_{fixtures.VERSION}_android-aarch64.apk')
        self.ipa=self.root/f'CrispSorter_{fixtures.VERSION}_ios-arm64.ipa';fixtures.ipa(self.ipa)
        self.draft=fixtures.Draft(self.root)

    def run_check(self):
        with patch.object(sys,'argv',['check','--repo','example/app','--tag','v'+fixtures.VERSION,'--version',fixtures.VERSION]), \
                patch.object(assets,'gh',side_effect=self.draft.gh),patch.object(gate,'android_manifest') as android:
            gate.main();android.assert_called_once()

    def test_complete_signed_package_layout_needs_no_receipts(self):self.run_check()

    def test_actual_ipa_version_not_report_field_is_checked(self):
        fixtures.ipa(self.ipa,'0.14.0');self.draft.refresh()
        with self.assertRaises(ValueError):self.run_check()

    def test_missing_or_unsigned_mobile_download_fails(self):
        for mode in ['missing','unsigned']:
            self.draft.refresh()
            if mode=='missing':self.draft.assets.pop()
            else:self.draft.assets.append({'name':'CrispSorter_ios-unsigned.app.tar.gz','size':1})
            with self.subTest(mode=mode),self.assertRaises(ValueError):self.run_check()

    def test_changed_download_bytes_fail_before_package_inspection(self):
        self.ipa.write_bytes(b'changed')
        with self.assertRaises(ValueError):self.run_check()


if __name__=='__main__':unittest.main()
