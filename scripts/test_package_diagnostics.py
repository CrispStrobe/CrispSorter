"""Optional reports never mask actual package-test failures or gate success."""
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch
import verify_release_artifacts as verifier


class PackageDiagnostics(unittest.TestCase):
    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory();self.addCleanup(self.tmp.cleanup);self.root=Path(self.tmp.name)
        (self.root/'src-tauri').mkdir();(self.root/'src-tauri/tauri.conf.json').write_text('{"version":"0.15.3"}')
        folder=self.root/'target/release/bundle/deb';folder.mkdir(parents=True)
        (folder/'CrispSorter_0.15.3_amd64.deb').write_bytes(b'fixture')

    def execute(self,version='0.15.3',failure=None):
        with patch.object(verifier.Path,'cwd',return_value=self.root),patch.object(sys,'argv',['verify','--platform','linux-x86_64']), \
                patch.dict(os.environ,{'CRISPSORTER_REQUIRE_DESKTOP_SIGNING':'0'}), \
                patch.object(verifier,'inspect_package',return_value={'checks':[{'result':{'version':version}}]},side_effect=failure):
            verifier.main()

    def test_unwritable_diagnostics_do_not_fail_verified_package(self):
        with patch.object(Path,'write_text',side_effect=OSError('private error')):self.execute()

    def test_real_verifier_failure_is_still_fatal_without_reports(self):
        with patch.object(Path,'write_text',side_effect=OSError('private error')):
            with self.assertRaises(ValueError):self.execute(failure=ValueError('actual package failure'))
            with self.assertRaises(ValueError):self.execute(version='0.14.0')

    def test_no_git_toolchain_or_source_receipt_prerequisite(self):
        with patch.object(verifier.subprocess,'check_output',side_effect=AssertionError('No metadata commands expected')):self.execute()

    def test_report_upload_optional_but_package_upload_not_optional(self):
        text=(Path(__file__).resolve().parents[1]/'.github/workflows/release.yml').read_text()
        self.assertIn('python scripts/verify_release_artifacts.py',text)
        for line in text.splitlines():
            if 'gh release upload ' not in line:continue
            if '.receipt.json' in line or '.signing.json' in line:
                self.assertIn('|| echo',line)
            if 'gh release upload "${TAG}" "$ANDROID_RELEASE_APK" --clobber' in line:
                self.assertNotIn('||',line)

class SigningDiagnostics(unittest.TestCase):
    def test_signed_apk_survives_optional_report_write_failure(self):
        import shutil
        import sign_android_apk as signer
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory);source=root/'input.apk';output=root/'output.apk';source.write_bytes(b'APK fixture')
            def sign(command,**kwargs):shutil.copyfile(source,command[command.index('--out')+1])
            with patch.object(sys,'argv',['sign',str(source),str(output)]), \
                    patch.dict(os.environ,{'ANDROID_KEYSTORE':'fixture','ANDROID_KEY_ALIAS':'fixture','ANDROID_KEYSTORE_PASSWORD':'fixture','ANDROID_KEY_PASSWORD':'fixture'}), \
                    patch.object(signer,'check_alignment',return_value={'alignment':'unchanged'}), \
                    patch.object(signer,'signer_path',return_value='apksigner'), \
                    patch.object(signer.subprocess,'run',side_effect=sign), \
                    patch.object(signer.subprocess,'check_output',return_value='verified'), \
                    patch.object(Path,'write_text',side_effect=OSError('private report error')):
                signer.main()
            self.assertEqual(output.read_bytes(),source.read_bytes())


if __name__=='__main__':unittest.main()
