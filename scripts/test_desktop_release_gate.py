"""Actual desktop package fixtures, independent of source/receipt paperwork."""
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch
import check_desktop_release_assets as gate
import release_package_assets as assets
import test_release_package_assets as fixtures


class DesktopPackages(unittest.TestCase):
    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory();self.addCleanup(self.tmp.cleanup);self.root=Path(self.tmp.name)
        self.deb=self.root/f'CrispSorter_{fixtures.VERSION}_amd64.deb';self.deb.write_bytes(b'DEB fixture inspected by dpkg-deb')
        (self.root/f'CrispSorter_{fixtures.VERSION}_aarch64.dmg').write_bytes(b'koly'+b'\0'*508)
        fixtures.mac(self.root/'CrispSorter_aarch64.app.tar.gz')
        fixtures.windows(self.root/f'CrispSorter_{fixtures.VERSION}_x64-windows-portable.zip')
        self.draft=fixtures.Draft(self.root)

    def run_check(self,metadata=None):
        def dpkg(*args,**kwargs):
            if isinstance(metadata,Exception):raise metadata
            return metadata or 'Version: 0.15.3\nArchitecture: amd64\n'
        with patch.object(sys,'argv',['check','--repo','example/app','--tag','v'+fixtures.VERSION,'--version',fixtures.VERSION]), \
                patch.object(assets,'gh',side_effect=self.draft.gh),patch.object(gate.subprocess,'check_output',side_effect=dpkg):
            gate.main()

    def test_complete_packages_pass_without_any_receipts(self):self.run_check()

    def test_wrong_or_malformed_deb_metadata_fails(self):
        for value in ['Version: 0.14.0\nArchitecture: amd64\n','Version: 0.15.3\nArchitecture: arm64\n','malformed deb']:
            with self.subTest(value=value),self.assertRaises(ValueError):self.run_check(value)
        with self.assertRaises(subprocess.CalledProcessError):self.run_check(subprocess.CalledProcessError(2,['dpkg-deb']))

    def test_unsigned_and_missing_packages_are_not_publishable(self):
        for mode in ['missing','unsigned']:
            self.draft.refresh()
            if mode=='missing':self.draft.assets.pop()
            else:
                asset=next(a for a in self.draft.assets if a['name'].endswith('.dmg'))
                asset['name']=asset['name'].replace('.dmg','-unsigned.dmg')
            with self.subTest(mode=mode),self.assertRaises(ValueError):self.run_check()

    def test_successful_required_jobs_and_signing_remain_mandatory(self):
        text=(Path(__file__).resolve().parents[1]/'.github/workflows/release.yml').read_text()
        publish=text.split('\n  publish:',1)[1]
        for job in ['release-required','release-android','release-ios']:
            self.assertIn('needs.'+job+".result == 'success'",publish)
        self.assertIn('CRISPSORTER_REQUIRE_DESKTOP_SIGNING=1',text)
        self.assertIn('codesign --verify --deep --strict',text)
        self.assertIn('python scripts/sign_android_apk.py',text)
        self.assertIn("packages: 'build-tools;36.0.0'",publish)
        self.assertIn('echo \"$ANDROID_HOME/build-tools/36.0.0\" >> \"$GITHUB_PATH\"',publish)
        self.assertNotIn('--source',publish)


if __name__=='__main__':unittest.main()
