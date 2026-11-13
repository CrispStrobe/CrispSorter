#!/usr/bin/env python3
"""Exercise draft staging/provenance without compiling, signing, or contacting services."""
import hashlib
import json
import os
from pathlib import Path
import plistlib
import re
import tempfile
import unittest
from unittest.mock import patch
import zipfile

import prepare_signed_mobile_ocr_draft as staging
import record_signed_mobile_ocr_draft as recording
from check_mobile_release_profile import profile


def context(target='android-arm64', **overrides):
    value = {'GITHUB_EVENT_NAME': 'workflow_dispatch', 'CRISPSORTER_RELEASE_PUBLISH': 'false',
             'CRISPSORTER_RELEASE_MAC_APPSTORE': 'false',
             'CRISPSORTER_RELEASE_SELECTOR': 'android_ocr_draft' if target == 'android-arm64' else 'ios_ocr_draft'}
    return {**value, **overrides}


class DraftStaging(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.base = Path(temporary.name)
        self.project = self.base / 'repos/CrispSorter'
        (self.project / 'scripts').mkdir(parents=True)
        (self.project / 'src-tauri').mkdir()
        self.output = self.base / 'draft'
        self.lock = {'source_commit': 'production', 'ggml_commit': 'production-ggml'}
        for filename in [staging.PRODUCTION_LOCK, staging.RUNTIME_LOCK]:
            (self.project / filename).write_text(json.dumps(self.lock))
        self.manifest = self.project / 'src-tauri/Cargo.toml'
        self.manifest.write_text('committed fixture manifest')

    def native(self, source, output, target, jobs, ndk, lock):
        self.assertEqual(jobs, 2)
        self.assertEqual(lock, self.project / staging.RUNTIME_LOCK)
        (output / 'lib').mkdir(parents=True)
        (output / 'lib' / ('libcrispembed.so' if target.startswith('android-') else 'libcrispembed.a')).write_bytes(b'native fixture')
        (output / 'notices').mkdir()
        (output / 'notices/native-LICENSE.txt').write_text('notice')
        (output / 'runtime-build.json').write_text('{}')

    def ort(self, command, **kwargs):
        self.assertIn('prepare_mobile_ort.py', command[1])
        root = Path(command[command.index('--output') + 1])
        (root / 'notices').mkdir(parents=True)
        for name in ['LICENSE', 'ThirdPartyNotices.txt']:
            (root / 'notices' / name).write_text('notice')
        (root / 'ort-runtime.json').write_text('{}')

    def prepare(self, target='android-arm64', **overrides):
        with patch.dict(os.environ, context(target, **overrides)), \
                patch.object(staging, 'select_lock', return_value={'source': 'production'}), \
                patch.object(staging, 'check_layout', return_value={'app_source': 'application'}), \
                patch.object(staging, 'check_versions', return_value='0.15.1'), \
                patch.object(staging, 'build_runtime', side_effect=self.native) as native, \
                patch.object(staging.subprocess, 'run', side_effect=self.ort), \
                patch.object(staging, 'prepare_cargo') as cargo:
            result = staging.prepare(self.project, self.output, target, 'production', 'asr', 'docx', Path('/host/protoc'))
        return result, native, cargo

    def test_stages_each_device_target_without_rewriting_manifest(self):
        for target in ['android-arm64', 'ios-arm64']:
            with self.subTest(target=target):
                self.output = self.base / target
                environment, native, cargo = self.prepare(target)
                self.assertEqual(environment['LICENSES_CARGO_FEATURES'], 'ocr-mobile,tauri/custom-protocol')
                self.assertEqual(environment['CARGO_BUILD_JOBS'], '2')
                self.assertEqual(self.manifest.read_text(), 'committed fixture manifest')
                self.assertEqual((self.project / 'src-tauri/bin/ocr-mobile-runtime.json').read_text(), '{}')
                self.assertTrue((self.project / 'src-tauri/bin/onnxruntime-LICENSE.txt').is_file())
                self.assertEqual(cargo.call_args.kwargs['ort_lib_dir'], self.output / 'ort/lib' if target.startswith('android-') else None)
                self.assertEqual(cargo.call_args.kwargs['ort_xcframework'], self.output / 'ort/onnxruntime.xcframework' if target.startswith('ios-') else None)
                self.assertEqual(json.loads((self.output / 'preparation.json').read_text())['qualification'], 'Not established; build preparation only')
                native.assert_called_once()

    def test_publish_tag_or_mas_context_rejected_before_native_build(self):
        for invalid in [{'CRISPSORTER_RELEASE_PUBLISH': 'true'}, {'GITHUB_EVENT_NAME': 'push'},
                        {'CRISPSORTER_RELEASE_MAC_APPSTORE': 'true'}, {'CRISPSORTER_RELEASE_SELECTOR': 'all'}]:
            with self.subTest(invalid=invalid), self.assertRaisesRegex(ValueError, 'explicit manual draft'):
                self.prepare(**invalid)
        self.assertFalse(self.output.exists())

    def test_existing_output_refused(self):
        self.output.mkdir()
        with self.assertRaisesRegex(ValueError, 'fresh draft'):
            self.prepare()

    def test_staging_inside_dependency_source_is_rejected_before_build(self):
        self.output = self.project.parent / 'CrispEmbed/generated'
        with self.assertRaisesRegex(ValueError, 'outside the application and dependency'):
            self.prepare()
        self.assertFalse(self.output.exists())

    def test_candidate_runtime_refused(self):
        with patch.dict(os.environ, context()), \
                patch.object(staging, 'select_lock', side_effect=[{'source': 'production'}, {'source': 'candidate'}]), \
                patch.object(staging, 'build_runtime') as build:
            with self.assertRaisesRegex(ValueError, 'production source pin'):
                staging.prepare(self.project, self.output, 'android-arm64', 'production', 'asr', 'docx', Path('/host/protoc'))
            build.assert_not_called()


class DraftRecording(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.base = Path(temporary.name)
        self.project = self.base / 'app'
        (self.project / 'scripts').mkdir(parents=True)
        (self.project / 'src-tauri').mkdir()
        (self.project / staging.RUNTIME_LOCK).write_text('{}')
        (self.project / 'src-tauri/tauri.conf.json').write_text('{"version":"0.15.1"}')
        self.root = self.base / 'draft'
        (self.root / 'runtime').mkdir(parents=True)
        (self.root / 'ort').mkdir()
        for name in ['layout.json', 'runtime/runtime-build.json', 'ort/ort-runtime.json']:
            (self.root / name).write_text('{}')
        self.build = {'sorter_commit': 'application', 'crispembed_commit': 'production'}

    def artifact(self, target):
        path = self.root / ('signed.apk' if target.startswith('android-') else 'signed.ipa')
        preparation = {'target': target, 'app_source': 'application', 'production_source': 'production',
                       'runtime_lock': staging.RUNTIME_LOCK, 'version': '0.15.1',
                       'runtime_lock_sha256': hashlib.sha256(b'{}').hexdigest()}
        (self.root / 'preparation.json').write_text(json.dumps(preparation))
        if target.startswith('android-'):
            path.write_bytes(b'APK fixture')
            path.with_suffix('.signing.json').write_text(json.dumps({'signing': 'provided production certificate',
                'sha256': recording.sha(path), 'filename': path.name}))
        else:
            with zipfile.ZipFile(path, 'w') as archive:
                archive.writestr('Payload/App.app/embedded.mobileprovision', b'profile fixture')
                archive.writestr('Payload/App.app/Info.plist', plistlib.dumps({'CFBundleShortVersionString': '0.15.1'}))
        return path

    def command(self, command, **kwargs):
        if command[0] == 'unzip':
            with zipfile.ZipFile(command[2]) as archive:
                archive.extractall(command[4])

    def run_record(self, target, artifact, build=None, failure=None):
        with patch.dict(os.environ, context(target)), patch.object(recording, 'signer_path', return_value='apksigner'), \
                patch.object(recording.subprocess, 'run', side_effect=self.command), \
                patch.object(recording, 'record', return_value=build or self.build, side_effect=failure) as record:
            result = recording.record_draft(self.project, self.root, target, artifact)
        return result, record

    def test_android_final_signed_bytes_bound_to_native_provenance(self):
        artifact = self.artifact('android-arm64')
        result, record = self.run_record('android-arm64', artifact)
        self.assertEqual(result['sha256'], recording.sha(artifact))
        self.assertEqual(result['validation_status'], 'passed')
        self.assertIn('Not established', result['qualification'])
        self.assertEqual(record.call_args.args[1], 'android')
        self.assertEqual(record.call_args.args[-1], staging.RUNTIME_LOCK)
        self.assertEqual(set(p.name for p in (self.root / 'artifact').iterdir()), {
            result['filename'], 'android.signing.json', 'build-receipt.json', 'signed-draft.json',
            'preparation.json', 'layout.json', 'runtime-build.json', 'ort-runtime.json'})

    def test_ios_final_signed_archive_binds_device_app_receipt(self):
        artifact = self.artifact('ios-arm64')
        result, record = self.run_record('ios-arm64', artifact)
        self.assertEqual(record.call_args.args[1], 'ios-device')
        self.assertEqual(result['sha256'], recording.sha(artifact))
        self.assertEqual(result['validation_status'], 'passed')

    def test_native_provenance_failure_keeps_only_explicitly_unqualified_artifact(self):
        artifact = self.artifact('android-arm64')
        with self.assertRaisesRegex(ValueError, 'native mismatch'):
            self.run_record('android-arm64', artifact, failure=ValueError('native mismatch'))
        result = self.root / 'artifact'
        self.assertFalse((result / 'signed-draft.json').exists())
        self.assertFalse((result / 'build-receipt.json').exists())
        self.assertEqual(json.loads((result / 'failure.json').read_text())['validation_status'], 'failed')
        self.assertEqual(len(list(result.glob('*-unqualified.apk'))), 1)

    def test_preparation_source_mismatch_cannot_produce_success_marker(self):
        artifact = self.artifact('android-arm64')
        with self.assertRaisesRegex(ValueError, 'differs from prepared'):
            self.run_record('android-arm64', artifact, build={'sorter_commit': 'different', 'crispembed_commit': 'production'})
        self.assertFalse((self.root / 'artifact/signed-draft.json').exists())
        self.assertTrue((self.root / 'artifact/failure.json').is_file())

    def test_development_signature_is_not_accepted_as_signed_draft(self):
        artifact = self.artifact('android-arm64')
        value = json.loads(artifact.with_suffix('.signing.json').read_text())
        value['signing'] = 'development certificate'
        artifact.with_suffix('.signing.json').write_text(json.dumps(value))
        with self.assertRaisesRegex(ValueError, 'production Android signing evidence'):
            self.run_record('android-arm64', artifact)
        self.assertFalse((self.root / 'artifact/signed-draft.json').exists())


class WorkflowSafety(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.workflow = (Path(__file__).resolve().parents[1] / '.github/workflows/release.yml').read_text()

    def job(self, name, end):
        return self.workflow.split('\n  ' + name + ':', 1)[1].split('\n  ' + end + ':', 1)[0]

    def test_draft_selectors_never_admit_public_ocr_profile(self):
        for target in ['android-arm64', 'ios-arm64']:
            with patch.dict(os.environ, context(target)):
                self.assertEqual(profile('ocr-draft')['ocr_qualification'], 'not established')
                with self.assertRaisesRegex(ValueError, 'not implemented'):
                    profile('ocr')

    def test_drafts_skip_release_creation_and_all_external_package_uploads(self):
        preflight = self.job('release-draft-preflight', 'release-required')
        self.assertIn("if: env.CRISPSORTER_MOBILE_RELEASE_PROFILE != 'ocr-draft'", preflight)
        for name, end in [('release-android', 'release-ios'), ('release-ios', 'release-macos-appstore')]:
            job = self.job(name, end)
            self.assertIn("&& !inputs.publish", job)
            self.assertIn("!inputs.mac_appstore", job)
            for step in re.split(r'\n      - ', job)[1:]:
                if 'gh release upload' in step or 'altool --upload-package' in step:
                    self.assertIn("env.CRISPSORTER_MOBILE_RELEASE_PROFILE != 'ocr-draft'", step)
            if name == 'release-ios':
                self.assertIn("if: env.CRISPSORTER_MOBILE_RELEASE_PROFILE != 'ocr-draft' && env.IPA_UPLOADED == 'true'", job)
            self.assertIn('signed-ocr-draft-', job)
            self.assertIn('-unqualified', job)
        mas = self.job('release-macos-appstore', 'publish')
        self.assertIn("inputs.platform != 'android_ocr_draft' && inputs.platform != 'ios_ocr_draft'", mas)
        publish = self.workflow.split('\n  publish:', 1)[1]
        self.assertIn("needs.release-required.result == 'success'", publish)
        self.assertIn("inputs.platform == 'all'", self.job('release-required', 'release-windows'))

    def condition(self, job, values, event='workflow_dispatch'):
        line = next(line for line in job.splitlines() if line.startswith('    if: '))
        expression = line.split('if: ', 1)[1].strip().removeprefix('${{').removesuffix('}}').strip()
        expression = expression.replace('github.event_name', 'event').replace('github.ref', 'ref')
        expression = re.sub(r'inputs\.([a-z_]+)', lambda match: repr(values[match[1]]), expression)
        expression = expression.replace('&&', ' and ').replace('||', ' or ')
        expression = re.sub(r'!(?!=)', ' not ', expression)
        return eval(expression, {'__builtins__': {}, 'startsWith': lambda value, prefix: value.startswith(prefix)},
                    {'event': event, 'ref': 'refs/heads/fixture'})

    def test_contradictory_draft_inputs_cannot_start_other_builds_or_distribution(self):
        for selector in ['android_ocr_draft', 'ios_ocr_draft']:
            for publish in [False, True]:
                for mas in [False, True]:
                    values = {'platform': selector, 'publish': publish, 'mac_appstore': mas}
                    with self.subTest(values=values):
                        self.assertTrue(self.condition(self.job('release-draft-preflight', 'release-required'), values))
                        for name, end, expected in [('release-android', 'release-ios', 'android_ocr_draft'),
                                                    ('release-ios', 'release-macos-appstore', 'ios_ocr_draft')]:
                            self.assertEqual(self.condition(self.job(name, end), values),
                                             selector == expected and not publish and not mas)
                        self.assertFalse(self.condition(self.job('release-required', 'release-windows'), values))
                        self.assertFalse(self.condition(self.job('release-macos-appstore', 'publish'), values))
                        with patch.dict(os.environ, context('android-arm64' if selector.startswith('android') else 'ios-arm64',
                                CRISPSORTER_RELEASE_PUBLISH=str(publish).lower(),
                                CRISPSORTER_RELEASE_MAC_APPSTORE=str(mas).lower())):
                            if publish or mas:
                                with self.assertRaises(ValueError):
                                    profile('ocr-draft')
                            else:
                                self.assertEqual(profile('ocr-draft')['recipe'], 'ocr-mobile-draft')

    def test_standard_release_jobs_remain_available(self):
        values = {'platform': 'all', 'publish': False, 'mac_appstore': False}
        for name, end in [('release-android', 'release-ios'), ('release-ios', 'release-macos-appstore'),
                          ('release-required', 'release-windows')]:
            self.assertTrue(self.condition(self.job(name, end), values))

    def test_android_draft_forwards_locked_to_cargo_without_changing_standard_build(self):
        android = self.job('release-android', 'release-ios')
        self.assertIn('npx tauri android build --target aarch64 --features ocr-mobile --apk --ci --config src-tauri/tauri.ocr-mobile.conf.json -- --locked', android)
        self.assertIn('else\n            npx tauri android build --target aarch64\n', android)
        self.assertIn('node scripts/native_license_inventory.mjs verify', android)

    def test_source_and_license_checks_precede_native_profile_build(self):
        self.assertIn('Record locked native OCR draft licenses', self.workflow)
        self.assertIn('--features ocr-mobile', self.workflow)
        self.assertIn('--require-crispembed', self.workflow)
        self.assertIn('onnxruntime.xcframework/ios-arm64/', self.workflow)
        self.assertIn('record_signed_mobile_ocr_draft.py', self.workflow)
        self.assertIn("CRISPSORTER_RELEASE_PUBLISH: ${{ github.event_name != 'workflow_dispatch' || inputs.publish }}", self.workflow)


if __name__ == '__main__':
    unittest.main()
