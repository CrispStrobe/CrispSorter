#!/usr/bin/env python3
"""Source-bound IPA selection and upload-only workflow regressions; no credentials."""
import hashlib
import contextlib
import io
import json
from pathlib import Path
import plistlib
import shutil
import tempfile
import unittest
from unittest.mock import patch
import zipfile

import prepare_testflight_current as current

SOURCE = 'a' * 40
OTHER = 'b' * 40
REPO = 'Example/Project'
VERSION = '0.15.1'
IPA = f'CrispSorter_{VERSION}_ios-arm64.ipa'
RECEIPT = f'CrispSorter_{VERSION}_ios-arm64.receipt.json'


class CurrentTestFlight(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.project = self.root / 'project'
        (self.project / 'src-tauri').mkdir(parents=True)
        self.json(self.project / 'package.json', {'name': 'tauri-app', 'version': VERSION})
        self.json(self.project / 'package-lock.json', {'name': 'tauri-app', 'version': VERSION,
                 'packages': {'': {'name': 'tauri-app', 'version': VERSION}}})
        self.json(self.project / 'src-tauri/tauri.conf.json', {'version': VERSION})
        (self.project / 'src-tauri/Cargo.toml').write_text(f'[package]\nname="crispsorter"\nversion="{VERSION}"\n')
        (self.project / 'Cargo.lock').write_text(f'[[package]]\nname="crispsorter"\nversion="{VERSION}"\n')
        self.assets = self.root / 'assets'
        self.assets.mkdir()
        self.write_ipa()
        self.receipt = {'schema': 'crispsorter-release-receipt-v1', 'platform': 'ios-arm64',
            'version': VERSION, 'app_source': SOURCE,
            'mobile_release_profile': {'recipe': 'standard', 'requested': 'standard'},
            'artifacts': [{'filename': IPA, 'version': VERSION, 'bundle_id': 'com.crispstrobe.crispsorter',
                'embedded_profile': True, 'bytes': (self.assets / IPA).stat().st_size,
                'sha256': hashlib.sha256((self.assets / IPA).read_bytes()).hexdigest()}]}
        self.release = {'isDraft': True, 'targetCommitish': SOURCE, 'assets': [{'name': IPA}, {'name': RECEIPT}]}
        self.ref = None
        self.calls = []
        self.output = self.root / 'selected IPA'

    @staticmethod
    def json(path, value):
        path.write_text(json.dumps(value))

    def write_ipa(self, version=VERSION, bundle='com.crispstrobe.crispsorter', profile=True):
        with zipfile.ZipFile(self.assets / IPA, 'w') as archive:
            archive.writestr('Payload/CrispSorter.app/Info.plist', plistlib.dumps({
                'CFBundleIdentifier': bundle, 'CFBundleShortVersionString': version,
                'CFBundleVersion': '42', 'CFBundleExecutable': 'CrispSorter'}))
            archive.writestr('Payload/CrispSorter.app/CrispSorter', b'executable fixture; not signature evidence')
            if profile:
                archive.writestr('Payload/CrispSorter.app/embedded.mobileprovision', b'profile fixture')

    def target_gh(self, *args, **kwargs):
        self.calls.append(args)
        if args[:2] == ('release', 'view'):
            return self.release.copy()
        self.assertEqual(args[:2], ('api', f'repos/{REPO}/git/ref/tags/v{VERSION}'))
        return self.ref

    def gh(self, *args):
        self.calls.append(args)
        if args[:2] == ('release', 'view'):
            return json.dumps(self.release)
        self.assertEqual(args[:2], ('release', 'download'))
        self.assertEqual(args[2], 'v' + VERSION)
        name = args[args.index('--pattern') + 1]
        directory = Path(args[args.index('--dir') + 1])
        self.assertIn(name, [RECEIPT, IPA])
        shutil.copyfile(self.assets / name, directory / name)
        return ''

    def prepare(self, **kwargs):
        self.json(self.assets / RECEIPT, self.receipt)
        with patch('check_release_target.gh', side_effect=self.target_gh), patch.object(current, 'gh', side_effect=self.gh):
            return current.prepare(REPO, SOURCE, self.project, self.output, **kwargs)

    def test_valid_current_source_selection_uses_only_reads_and_exact_downloads(self):
        selected = self.prepare()
        self.assertEqual(selected['source'], SOURCE)
        self.assertEqual(selected['version'], VERSION)
        self.assertEqual(selected['build_number'], '42')
        self.assertEqual(Path(selected['ipa']).read_bytes(), (self.assets / IPA).read_bytes())
        self.assertEqual(selected['sha256'], self.receipt['artifacts'][0]['sha256'])
        downloads = [args[args.index('--pattern') + 1] for args in self.calls if args[:2] == ('release', 'download')]
        self.assertEqual(downloads, [RECEIPT, IPA])
        self.assertTrue(all(args[0] == 'api' or args[:2] in [('release', 'view'), ('release', 'download')] for args in self.calls))
        environment = self.root / 'github-env'
        current.write_environment(selected, environment)
        self.assertIn('IPA_VERSION=0.15.1\n', environment.read_text())
        self.assertNotIn('BUILD_NUMBER=', environment.read_text())  # Derived after platform signature verification.

    def test_publication_flags_are_rejected_before_any_github_access_or_environment_write(self):
        for flags in [{'publish': True}, {'mac_appstore': True}, {'publish': True, 'mac_appstore': True}]:
            with self.subTest(flags=flags), self.assertRaisesRegex(ValueError, 'publish=false'):
                self.prepare(**flags)
            self.assertEqual(self.calls, [])
            self.assertFalse(self.output.exists())

    def test_workflow_cli_cannot_write_selection_environment_on_an_invalid_request(self):
        environment = self.root / 'github-env'
        environment.write_text('KEEP=existing\n')
        arguments = ['prepare', '--repo', REPO, '--source', SOURCE, '--project', str(self.project),
            '--output', str(self.output), '--publish', 'true', '--mac-appstore', 'false', '--github-env', str(environment)]
        with patch('sys.argv', arguments), contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as error:
            current.main()
        self.assertEqual(error.exception.code, 1)
        self.assertEqual(environment.read_text(), 'KEEP=existing\n')
        self.assertFalse(self.output.exists())

    def test_public_wrong_draft_target_or_wrong_tag_commit_are_rejected(self):
        for change in ['public', 'target', 'tag']:
            with self.subTest(change=change):
                self.release['isDraft'] = change != 'public'
                self.release['targetCommitish'] = OTHER if change == 'target' else SOURCE
                self.ref = {'object': {'type': 'commit', 'sha': OTHER}} if change == 'tag' else None
                with self.assertRaises(ValueError):
                    self.prepare()
                self.assertFalse(self.output.exists())

    def test_missing_receipt_or_ipa_is_not_replaced_with_older_assets(self):
        for missing in [RECEIPT, IPA]:
            with self.subTest(missing=missing):
                self.release['assets'] = [{'name': name} for name in [RECEIPT, IPA] if name != missing]
                self.release['assets'].append({'name': 'CrispSorter_0.15.0_ios-arm64.ipa'})
                with self.assertRaisesRegex(ValueError, 'exactly one current'):
                    self.prepare()
                self.assertFalse(self.output.exists())

    def test_receipt_source_version_profile_identity_size_and_hash_tampering_fails(self):
        original = json.loads(json.dumps(self.receipt))
        changes = [lambda r: r.update(app_source=OTHER), lambda r: r.update(version='0.15.0'),
            lambda r: r.update(platform='android-arm64'), lambda r: r.update(schema='untrusted'),
            lambda r: r['mobile_release_profile'].update(recipe='ocr-mobile-draft'),
            lambda r: r['artifacts'][0].update(filename='../other.ipa'),
            lambda r: r['artifacts'][0].update(version='0.15.0'),
            lambda r: r['artifacts'][0].update(embedded_profile=False),
            lambda r: r['artifacts'][0].update(bundle_id='another.app'),
            lambda r: r['artifacts'][0].update(bytes=1),
            lambda r: r['artifacts'][0].update(sha256='0' * 64)]
        for change in changes:
            self.receipt = json.loads(json.dumps(original))
            change(self.receipt)
            with self.assertRaises((ValueError, KeyError)):
                self.prepare()
            self.assertFalse(self.output.exists())

    def test_resume_reuses_exact_current_ipa_and_original_cutoff(self):
        timestamp = '2026-11-12T12:00:00Z'
        selection = self.prepare(mode='resume', uploaded_after=timestamp)
        self.assertEqual(selection['mode'], 'resume')
        self.assertEqual(selection['source'], SOURCE)
        self.assertEqual(selection['build_number'], '42')
        self.assertEqual(selection['uploaded_after'], timestamp)
        environment = self.root / 'github-env'
        current.write_environment(selection, environment)
        self.assertIn('UPLOADED_AFTER=' + timestamp + '\n', environment.read_text())
        self.assertNotIn('BUILD_NUMBER=', environment.read_text())
        self.assertTrue(all(args[0] == 'api' or args[:2] in [('release', 'view'), ('release', 'download')] for args in self.calls))

    def test_resume_missing_malformed_naive_or_future_cutoff_fails_before_reads(self):
        for timestamp in [None, '', '2026-11-12T12:00:00', 'yesterday', '2026-11-12T12:00:00Z\nINJECT=value',
                          '2026-02-30T12:00:00Z', '2099-01-01T00:00:00Z']:
            with self.subTest(timestamp=timestamp), self.assertRaises(ValueError):
                self.prepare(mode='resume', uploaded_after=timestamp)
            self.assertEqual(self.calls, [])
            self.assertFalse(self.output.exists())

    def test_resume_still_rejects_source_and_ipa_mismatches(self):
        self.receipt['app_source'] = OTHER
        with self.assertRaisesRegex(ValueError, 'exact current source'):
            self.prepare(mode='resume', uploaded_after='2026-11-12T12:00:00Z')
        self.assertFalse(self.output.exists())
        self.receipt['app_source'] = SOURCE
        self.receipt['artifacts'][0]['sha256'] = '0' * 64
        with self.assertRaisesRegex(ValueError, 'SHA-256'):
            self.prepare(mode='resume', uploaded_after='2026-11-12T12:00:00Z')
        self.assertFalse(self.output.exists())

    def test_resume_publication_flags_still_fail_before_github_reads(self):
        for flags in [{'publish': True}, {'mac_appstore': True}]:
            with self.subTest(flags=flags), self.assertRaisesRegex(ValueError, 'publish=false'):
                self.prepare(mode='resume', uploaded_after='2026-11-12T12:00:00Z', **flags)
            self.assertEqual(self.calls, [])
            self.assertFalse(self.output.exists())

    def test_upload_cannot_borrow_an_old_resume_cutoff(self):
        with self.assertRaisesRegex(ValueError, 'only in resume mode'):
            self.prepare(uploaded_after='2026-11-12T12:00:00Z')
        self.assertEqual(self.calls, [])

    def test_ipa_tampering_and_falsely_matching_metadata_do_not_qualify(self):
        self.write_ipa(version='0.15.0')
        with self.assertRaises(ValueError):
            self.prepare()
        # Even a receipt updated to the wrong IPA's hash/size cannot hide its version.
        self.receipt['artifacts'][0].update(bytes=(self.assets / IPA).stat().st_size,
            sha256=hashlib.sha256((self.assets / IPA).read_bytes()).hexdigest())
        with self.assertRaisesRegex(ValueError, 'identity/version'):
            self.prepare()


class CurrentWorkflowIsolation(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.workflow = (Path(__file__).resolve().parents[1] / '.github/workflows/release.yml').read_text()

    def job(self, name, next_name):
        return self.workflow.split(f'\n  {name}:', 1)[1].split(f'\n  {next_name}:', 1)[0]

    def test_source_selection_and_signature_verification_precede_upload(self):
        job = self.job('testflight-existing', 'release-draft-preflight')
        self.assertLess(job.index('scripts/prepare_testflight_current.py'), job.index('codesign --verify --deep --strict'))
        self.assertLess(job.index('codesign --verify --deep --strict'), job.index('xcrun altool --upload-package'))
        self.assertIn('--source "$(git rev-parse HEAD)"', job)
        self.assertIn("--publish '${{ inputs.publish }}' --mac-appstore '${{ inputs.mac_appstore }}'", job)
        self.assertIn('--sha256 "$IPA_SHA256"', job)
        self.assertLess(job.index('echo "UPLOADED_AFTER='), job.index('xcrun altool --upload-package'))
        self.assertIn('--version "$BUILD_NUMBER" --uploaded-after "$UPLOADED_AFTER"', job)

    def test_current_resume_cannot_reupload_and_preserves_explicit_cutoff(self):
        job = self.job('testflight-existing', 'release-draft-preflight')
        upload = job.split('      - name: Upload to App Store Connect', 1)[1].split('      - name:', 1)[0]
        condition = upload.split('        if: ', 1)[1].split('\n', 1)[0]
        for selector, expected in [('testflight_current_resume', False), ('testflight_current', True), ('testflight', True)]:
            expression = condition.replace('inputs.platform', repr(selector)).replace('||', ' or ')
            self.assertEqual(eval(expression, {'__builtins__': {}}), expected)
        selection = job.split('      - name: Select the exact current-source draft IPA', 1)[1].split('      - name:', 1)[0]
        self.assertIn("CURRENT_TESTFLIGHT_MODE: ${{ inputs.platform == 'testflight_current_resume' && 'resume' || 'upload' }}", selection)
        self.assertIn("ORIGINAL_UPLOAD_TIMESTAMP: ${{ inputs.testflight_uploaded_after || '' }}", selection)
        self.assertIn('--mode "$CURRENT_TESTFLIGHT_MODE" --uploaded-after "$ORIGINAL_UPLOAD_TIMESTAMP"', selection)
        self.assertNotIn("${{ inputs.testflight_uploaded_after }}", selection.split('run: |', 1)[1])
        self.assertIn('--marketing-version "$IPA_VERSION"', job)
        appstore = self.job('release-macos-appstore', 'publish')
        self.assertIn("inputs.platform != 'testflight_current_resume'", appstore.split('permissions:', 1)[0])
        self.assertIn('Set the $IPA_VERSION upload timestamp before resuming.', job)

    def test_current_selector_cannot_start_build_or_public_release_jobs(self):
        for name, next_name in [('release-draft-preflight', 'release-required'), ('release-required', 'release-windows'),
                                ('release-android', 'release-ios'), ('release-ios', 'release-macos-appstore')]:
            condition = self.job(name, next_name).split('    if:', 1)[1].split('\n', 1)[0]
            self.assertNotIn('testflight_current', condition)
        appstore = self.workflow.split('\n  release-macos-appstore:', 1)[1].split('\n  publish:', 1)[0]
        self.assertIn("inputs.platform != 'testflight_current'", appstore.split('permissions:', 1)[0])
        self.assertIn("github.event_name != 'workflow_dispatch' && startsWith(github.ref, 'refs/tags/v')", appstore.split('permissions:', 1)[0])
        publish = self.workflow.split('\n  publish:', 1)[1]
        self.assertIn("needs.release-required.result == 'success'", publish)
        self.assertIn("needs.release-android.result == 'success'", publish)
        self.assertIn("needs.release-ios.result == 'success'", publish)


if __name__ == '__main__':
    unittest.main()
