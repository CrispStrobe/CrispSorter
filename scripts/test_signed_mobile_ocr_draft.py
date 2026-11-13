#!/usr/bin/env python3
"""Exercise draft staging/provenance without compiling, signing, or contacting services."""
import hashlib
import json
import os
from pathlib import Path
import plistlib
import re
import subprocess
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
        self.tmp=tempfile.TemporaryDirectory();self.addCleanup(self.tmp.cleanup)
        self.project=Path(self.tmp.name)/'app';self.root=Path(self.tmp.name)/'draft'
        (self.project/'src-tauri').mkdir(parents=True);self.root.mkdir()
        (self.project/'src-tauri/tauri.conf.json').write_text('{"version":"0.15.1"}')

    def artifact(self,target):
        path=self.root/('signed.apk' if target=='android-arm64' else 'signed.ipa')
        if target=='android-arm64':
            path.write_bytes(b'APK fixture')
            path.with_suffix('.signing.json').write_text(json.dumps({'signing':'provided production certificate',
                'sha256':recording.sha(path),'filename':path.name,'unused_secret':'SECRET-SENTINEL'}))
        else:
            with zipfile.ZipFile(path,'w') as archive:
                archive.writestr('Payload/App.app/embedded.mobileprovision',b'fixture')
                archive.writestr('Payload/App.app/Info.plist',plistlib.dumps({'CFBundleShortVersionString':'0.15.1'}))
        return path

    def command(self,command,**kwargs):
        if command[0]=='unzip':
            with zipfile.ZipFile(command[2]) as archive:archive.extractall(command[4])

    def execute(self,target,artifact,failure=None,badging=None):
        def output(command,**kwargs):
            if command[0]=='keytool':return b'production certificate'
            if command[1]=='verify':return 'Signer #1 certificate SHA-256 digest: '+hashlib.sha256(b'production certificate').hexdigest()+'\n'
            return badging or "package: name='com.crispstrobe.crispsorter' versionCode='1' versionName='0.15.1'\n"
        with patch.dict(os.environ,context(target,ANDROID_KEYSTORE='/sdk/configured.jks',ANDROID_KEY_ALIAS='configured')),patch.object(recording,'signer_path',return_value='/sdk/apksigner'), \
                patch.object(recording,'aapt_path',return_value='/sdk/aapt'), \
                patch.object(recording.subprocess,'run',side_effect=self.command) as commands, \
                patch.object(recording.subprocess,'check_output',side_effect=output) as outputs, \
                patch.object(recording,'check_native',side_effect=failure) as native:
            result=recording.record_draft(self.project,self.root,target,artifact)
        return result,commands,native

    def test_real_signature_and_native_checks_for_both_targets(self):
        for target in ['android-arm64','ios-arm64']:
            artifact=self.artifact(target)
            if target=='android-arm64':artifact.with_suffix('.signing.json').unlink()
            result,commands,native=self.execute(target,artifact)
            self.assertEqual(result['validation_status'],'passed')
            self.assertEqual(result['sha256'],recording.sha(artifact))
            native.assert_called_once();self.assertEqual(native.call_args.args[2],target)
            if target=='ios-arm64':self.assertTrue(any(c.args[0][0]=='codesign' for c in commands.call_args_list))
            self.assertEqual({p.name for p in (self.root/'artifact').iterdir()},{result['filename'],'signed-draft.json'})
            self.assertNotIn('SECRET-SENTINEL',(self.root/'artifact/signed-draft.json').read_text())

    def test_native_failure_replaces_previous_success_without_raw_exception(self):
        artifact=self.artifact('android-arm64');self.execute('android-arm64',artifact)
        with self.assertRaises(ValueError):self.execute('android-arm64',artifact,failure=ValueError('SECRET-SENTINEL'))
        self.assertFalse((self.root/'artifact/signed-draft.json').exists())
        self.assertTrue((self.root/'artifact/failure.json').exists())
        self.assertNotIn('SECRET-SENTINEL',(self.root/'artifact/failure.json').read_text())
        self.assertEqual(self.execute('android-arm64',artifact)[0]['validation_status'],'passed')

    def test_android_bundle_and_version_mismatch_fail(self):
        for badging in ["package: name='wrong.bundle' versionCode='1' versionName='0.15.1'",
                        "package: name='com.crispstrobe.crispsorter' versionCode='1' versionName='0.14.0'"]:
            artifact=self.artifact('android-arm64')
            with self.assertRaisesRegex(ValueError,'identifier or version'):self.execute('android-arm64',artifact,badging=badging)

    def test_ios_version_mismatch_fails_before_native_check(self):
        artifact=self.artifact('ios-arm64')
        (self.project/'src-tauri/tauri.conf.json').write_text('{"version":"0.15.2"}')
        with self.assertRaisesRegex(ValueError,'version differs'):self.execute('ios-arm64',artifact)

    def test_signature_failure_cannot_pass(self):
        artifact=self.artifact('android-arm64')
        with patch.object(recording.subprocess,'check_output',side_effect=subprocess.CalledProcessError(1,['signer'])), \
                patch.dict(os.environ,context()),patch.object(recording,'signer_path',return_value='/sdk/apksigner'):
            with self.assertRaises(subprocess.CalledProcessError):recording.record_draft(self.project,self.root,'android-arm64',artifact)

    def test_wrong_android_certificate_is_not_accepted_without_signing_report(self):
        artifact=self.artifact('android-arm64');artifact.with_suffix('.signing.json').unlink()
        def output(command,**kwargs):
            if command[0]=='keytool':return b'configured certificate'
            return 'Signer #1 certificate SHA-256 digest: '+'a'*64+'\n'
        with patch.object(recording.subprocess,'check_output',side_effect=output), \
                patch.object(recording,'signer_path',return_value='/sdk/apksigner'), \
                patch.dict(os.environ,context(ANDROID_KEYSTORE='/sdk/configured.jks',ANDROID_KEY_ALIAS='configured')):
            with self.assertRaisesRegex(ValueError,'signer differs'):
                recording.record_draft(self.project,self.root,'android-arm64',artifact)

    def test_report_write_is_optional_but_package_copy_is_required(self):
        artifact=self.artifact('android-arm64')
        with patch.object(recording.shutil,'copy2',side_effect=OSError('private')):
            with self.assertRaises(OSError):self.execute('android-arm64',artifact)
        with patch.object(Path,'write_text',side_effect=OSError('private')):
            self.assertEqual(self.execute('android-arm64',artifact)[0]['validation_status'],'passed')
        self.assertTrue((self.root/'artifact/CrispSorter_0.15.1_android-arm64-ocr-draft-unqualified.apk').is_file())

    def test_corrupt_package_copy_is_not_a_success(self):
        artifact=self.artifact('android-arm64')
        def corrupt(source,destination):Path(destination).write_bytes(b'corrupt package')
        with patch.object(recording.shutil,'copy2',side_effect=corrupt):
            with self.assertRaisesRegex(ValueError,'not retained intact'):
                self.execute('android-arm64',artifact)
        self.assertFalse((self.root/'artifact/signed-draft.json').exists())

    def test_no_preparation_or_clean_tree_or_json_schema_admission(self):
        artifact=self.artifact('android-arm64')
        for name in ['preparation.json','layout.json','runtime/runtime-build.json','ort/ort-runtime.json']:
            path=self.root/name;path.parent.mkdir(parents=True,exist_ok=True);path.write_text('SECRET-SENTINEL')
        (self.project/'local-edit').write_text('untracked working file')
        self.assertEqual(self.execute('android-arm64',artifact)[0]['validation_status'],'passed')
        for path in (self.root/'artifact').iterdir():self.assertNotIn(b'SECRET-SENTINEL',path.read_bytes())

    def test_native_checks_keep_target_architecture_ort_linkage_and_notices(self):
        (self.root/'runtime').mkdir();(self.project/'scripts').mkdir()
        runtime={'target':'android-arm64','source_commit':'a','ggml_commit':'b'}
        (self.root/'runtime/runtime-build.json').write_text(json.dumps(runtime))
        (self.project/staging.RUNTIME_LOCK).write_text(json.dumps(runtime))
        for target in ['android-arm64','ios-arm64']:
            runtime['target']=target;(self.root/'runtime/runtime-build.json').write_text(json.dumps(runtime))
            with patch.object(recording.native,'android_identity') as android, \
                    patch.object(recording.native,'bundle_identity') as ios, \
                    patch.object(recording.native,'ort_identity') as ort, \
                    patch.object(recording.native,'verify_native_provenance') as notices:
                recording.check_native(self.project,self.root,target,self.root/'package')
                (android if target=='android-arm64' else ios).assert_called_once()
                ort.assert_called_once();notices.assert_called_once()
                for verifier in [ort,notices]:
                    verifier.side_effect=ValueError('real package mismatch')
                    with self.assertRaises(ValueError):recording.check_native(self.project,self.root,target,self.root/'package')
                    verifier.side_effect=None
        runtime['target']='wrong';(self.root/'runtime/runtime-build.json').write_text(json.dumps(runtime))
        with self.assertRaises(ValueError):recording.check_native(self.project,self.root,'ios-arm64',self.root/'package')


class AndroidMetadataTool(unittest.TestCase):
    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory();self.addCleanup(self.tmp.cleanup)
        self.root=Path(self.tmp.name)

    def tool(self,name):
        path=self.root/name;path.parent.mkdir(parents=True,exist_ok=True)
        path.write_text('#!/bin/sh\n');path.chmod(0o755)
        return path

    def test_prefers_signer_sibling_over_path_and_sdk(self):
        sibling=self.tool('signer/aapt')
        with patch.object(recording.shutil,'which',return_value='/other/aapt') as which:
            self.assertEqual(recording.aapt_path(str(sibling.with_name('apksigner'))),str(sibling))
            which.assert_not_called()

    def test_system_signer_can_use_aapt_from_path(self):
        with patch.object(recording.shutil,'which',return_value='/sdk/aapt'):
            self.assertEqual(recording.aapt_path('/usr/bin/apksigner'),'/sdk/aapt')

    def test_system_signer_can_use_either_configured_sdk(self):
        for variable in ['ANDROID_HOME','ANDROID_SDK_ROOT']:
            sdk=self.root/variable
            self.tool(variable+'/build-tools/9.0.0/aapt')
            expected=self.tool(variable+'/build-tools/36.0.0/aapt')
            with patch.dict(os.environ,{'ANDROID_HOME':'','ANDROID_SDK_ROOT':'',variable:str(sdk)}), \
                    patch.object(recording.shutil,'which',return_value=None):
                self.assertEqual(recording.aapt_path('/usr/bin/apksigner'),str(expected))

    def test_missing_tool_reports_actual_sdk_requirement(self):
        with patch.dict(os.environ,{'ANDROID_HOME':'','ANDROID_SDK_ROOT':''}), \
                patch.object(recording.shutil,'which',return_value=None):
            with self.assertRaises(FileNotFoundError):recording.aapt_path(str(self.root/'missing/apksigner'))


class WorkflowSafety(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.workflow = (Path(__file__).resolve().parents[1] / '.github/workflows/release.yml').read_text()

    def job(self, name, end):
        return self.workflow.split('\n  ' + name + ':', 1)[1].split('\n  ' + end + ':', 1)[0]

    def test_packages_are_required_and_diagnostics_optional_without_replay_collection(self):
        self.assertNotIn('retain_ios_ocr_draft_inputs.py',self.workflow)
        self.assertNotIn('/signed-mobile-ocr-draft/replay-inputs',self.workflow)
        for target,suffix in [('android-arm64','apk'),('ios-arm64','ipa')]:
            marker='          name: signed-ocr-draft-'+target+'-unqualified'
            start=self.workflow.rindex('      - name: ',0,self.workflow.index(marker))
            package=self.workflow[start:].split('\n      - ',1)[0]
            self.assertNotIn('continue-on-error',package)
            self.assertIn('if: success()',package)
            self.assertIn('if-no-files-found: error',package)
            self.assertIn('/artifact/*.'+suffix,package)
            marker='          name: signed-ocr-draft-'+target+'-diagnostics'
            start=self.workflow.rindex('      - name: ',0,self.workflow.index(marker))
            diagnostic=self.workflow[start:].split('\n      - ',1)[0]
            self.assertIn('continue-on-error: true',diagnostic)
            self.assertIn('if-no-files-found: warn',diagnostic)
            self.assertIn('/artifact/signed-draft.json',diagnostic)
            self.assertIn('/artifact/failure.json',diagnostic)
        for target in ['Android','iOS']:
            step=self.workflow.split('      - name: Verify final signed '+target+' OCR draft provenance\n',1)[1].split('\n      - ',1)[0]
            self.assertIn('record_signed_mobile_ocr_draft.py',step)
            self.assertNotIn('continue-on-error',step)

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
