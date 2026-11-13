import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / 'scripts'))
from check_mobile_release_profile import profile
from check_desktop_release_assets import sha
from prepare_signed_mobile_ocr_draft import RUNTIME_LOCK, PRODUCTION_LOCK
from prepare_ocr_cargo_env import MARKER
from mobile_native_provenance import BASE_NOTICES
from ios_draft_retention_schema import ORT_FILES
import record_signed_mobile_ocr_draft as recording
import ios_draft_retention_schema as schema


def digest(data):
    return hashlib.sha256(data).hexdigest()


def git(project, *args):
    return subprocess.check_output(['git', '-C', str(project), *args], text=True).strip()


class IOSRecorderValidation(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name).resolve()
        self.project, self.stage = self.root / 'source', self.root / 'staging'
        self.project.mkdir()
        self.stage.mkdir()
        self.env = patch.dict(os.environ, {'GITHUB_EVENT_NAME': 'workflow_dispatch',
            'CRISPSORTER_RELEASE_PUBLISH': 'false', 'CRISPSORTER_RELEASE_MAC_APPSTORE': 'false',
            'CRISPSORTER_RELEASE_SELECTOR': 'ios_ocr_draft', 'GITHUB_RUN_ID': '123', 'GITHUB_RUN_ATTEMPT': '1'})
        self.env.start()
        self.addCleanup(self.env.stop)
        self.lock = {'source_commit': 'b'*40, 'ggml_commit': 'c'*40}
        for name in [RUNTIME_LOCK, PRODUCTION_LOCK]:
            self.write(self.project / name, self.lock)
        self.pin = {'url': 'https://example.invalid/pinned.zip', 'filename': 'pinned.zip', 'bytes': 10,
                    'sha256': 'a'*64, 'libraries': {'ios-arm64': {'bytes': 7, 'sha256': digest(b'fixture')}}}
        self.write(self.project / 'scripts/ocr-mobile-ort-lock.json', {'ios': self.pin, 'header_sha256': digest(b'fixture')})
        for name in ['Cargo.lock', 'Cargo.toml', 'src-tauri/Cargo.toml', 'static/licenses.json']:
            self.write(self.project / name, '{}')
        self.write(self.project / 'static/licenses.json', {'generatedAt': '2026-11-13T00:00:00Z',
            'counts': {'frontend': 0, 'backend': 0, 'total': 0}, 'licenses': []})
        self.write(self.project / 'src-tauri/tauri.conf.json', {'version': '0.15.1'})
        self.write(self.project / '.gitignore', '.cargo/\n')
        for args in [('init', '-q'), ('add', '.'), ('-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', 'commit', '-qm', 'fixture')]:
            subprocess.run(['git', '-C', str(self.project), *args], check=True, capture_output=True)
        commit = git(self.project, 'rev-parse', 'HEAD')
        self.write(self.stage / 'preparation.json', {'schema': 'crispsorter-signed-ocr-draft-preparation-v1',
            'profile': profile('ocr-draft'), 'qualification': 'Not established; build preparation only', 'target': 'ios-arm64', 'app_source': commit, 'version': '0.15.1',
            'production_source': self.lock['source_commit'], 'runtime_lock': RUNTIME_LOCK,
            'runtime_lock_sha256': sha(self.project / RUNTIME_LOCK)})
        self.write(self.stage / 'layout.json', {'schema': 'crispsorter-mobile-release-layout-v1', 'app_source': commit,
            'app_tree': git(self.project, 'rev-parse', 'HEAD^{tree}'), 'application_source_clean': True,
            'tracked_source_clean': True, 'generated_tracked_files': [], 'cargo_lock_sha256': sha(self.project / 'Cargo.lock'),
            'cargo_manifest_sha256': sha(self.project / 'src-tauri/Cargo.toml'),
            'dependency_sources': {'CrispEmbed': 'b'*40, 'CrispASR': 'd'*40, 'crisp-docx': 'e'*40},
            'checked_dependencies': ['crispembed'], 'mobile_qualification_lock_matches_release': True,
            'scope': 'Clean source layout and dependency identity only; no native build or behavior qualification'})
        notices = {name: {'source_repository': repo, 'source_path': path, 'git_blob': 'a'*40,
            'bytes': 7, 'sha256': digest(b'fixture')} for name, (repo, path) in BASE_NOTICES.items()}
        self.write(self.stage / 'runtime/runtime-build.json', {'target': 'ios-arm64', **self.lock,
            'artifacts': {'libcrispembed.a': digest(b'fixture')}, 'native_provenance': {'schema_version': 1, 'components': {}, 'notices': notices}})
        self.write(self.stage / 'runtime/lib/libcrispembed.a', 'fixture')
        for name in notices:
            self.write(self.stage / 'runtime/notices' / name, 'fixture')
        self.ort = {'schema_version': 1, 'target': 'ios-arm64', 'kind': 'ocr-mobile-ort-runtime',
            'version': '1.23.0', 'api_version': 23, 'rust_binding': '2.0.0-rc.11',
            'details': {'framework_identifier': 'ios-arm64', 'architectures': ['arm64'], 'static': True, 'minimum_os_version': '15.1'},
            'recorded_at': '2026-11-13T00:00:00Z',
            'lock_sha256': sha(self.project / 'scripts/ocr-mobile-ort-lock.json'),
            'archive': {key: self.pin[key] for key in ('url', 'filename', 'bytes', 'sha256')},
            'files': {name: {'bytes': 7, 'sha256': digest(b'fixture')} for name in ORT_FILES}}
        self.write(self.stage / 'ort/ort-runtime.json', self.ort)
        for name in ORT_FILES:
            self.write(self.stage / 'ort' / name, 'fixture')
        values = {'CRISPEMBED_SYS_LIB_DIR': str(self.stage / 'runtime/lib'), 'PROTOC': '/fixture/protoc',
                  'ORT_IOS_XCFWK_LOCATION': str(self.stage / 'ort/onnxruntime.xcframework')}
        self.write(self.project / '.cargo/config.toml', '\n\n' + MARKER + '\n' + '\n'.join(
            f'[env.{key}]\nvalue = {json.dumps(value)}\nforce = true\n' for key, value in values.items()))
        self.write(self.stage / 'cargo-metadata.json', {'schema_version': 1, 'kind': 'locked-native-license-metadata', 'source_commit': commit,
            'cargo_version': 'cargo 1.91.0 (abcd1234 2026-01-01)', 'backend_count': 5,
            'metadata': {'packages': []}, 'metadata_sha256': digest(b'{"packages":[]}'),
            'target': 'aarch64-apple-ios', 'default_features': False, 'features': ['ocr-mobile', 'tauri/custom-protocol'],
            **{key: sha(self.project / path) for key, path in [('cargo_lock_sha256', 'Cargo.lock'),
                ('workspace_manifest_sha256', 'Cargo.toml'), ('application_manifest_sha256', 'src-tauri/Cargo.toml')]}})
        self.write(self.stage / 'artifact/CrispSorter_0.15.1_ios-arm64-ocr-draft-unqualified.ipa', 'signed fixture')

    def write(self, path, value):
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(value) if isinstance(value, dict) else value)


    def test_original_ios_recorder_cannot_copy_rejected_secret_metadata(self):
        path = self.stage / 'preparation.json'
        value = json.loads(path.read_text())
        value['debug_secret'] = 'SECRET-SENTINEL'
        self.write(path, value)
        result = self.stage / 'result'
        result.mkdir()
        with patch.object(recording, 'record') as record:
            with self.assertRaises(ValueError):
                recording.validate(self.project, self.stage, 'ios-arm64', self.stage / 'signed.ipa', value,
                    profile('ocr-draft'), result, '0.15.1', self.stage / 'retained.ipa')
        record.assert_not_called()
        self.assertEqual({p.name for p in result.iterdir()}, {'failure.json'})
        self.assertNotIn('SECRET-SENTINEL', (result / 'failure.json').read_text())

    def test_failure_record_does_not_copy_exception_secret_text(self):
        result = self.stage / 'result'
        result.mkdir()
        with patch.object(recording, 'validate_inner', side_effect=ValueError('SECRET-SENTINEL')):
            with self.assertRaises(ValueError):
                recording.validate(self.project, self.stage, 'ios-arm64', self.stage / 'signed.ipa', {},
                    profile('ocr-draft'), result, '0.15.1', self.stage / 'retained.ipa')
        self.assertNotIn('SECRET-SENTINEL', (result / 'failure.json').read_text())

    def test_nested_native_component_unknown_fields_are_rejected(self):
        value = json.loads((self.stage / 'runtime/runtime-build.json').read_text())
        component = {'source_path': 'vendor/sheenbidi', 'source_tree': 'a'*40, 'upstream_commit': 'b'*40,
            'artifacts': {'libcrispembed.a': {'sha256': digest(b'fixture'), 'defined_symbols': list(schema.SHEEN_SYMBOLS)}},
            'linkage': 'Unity C implementation in the primary native library'}
        value['native_provenance']['components'] = {'sheenbidi': component}
        for name, (repo, path) in schema.SHEEN_NOTICES.items():
            value['native_provenance']['notices'][name] = {'source_repository': repo, 'source_path': path,
                'git_blob': 'a'*40, 'bytes': 7, 'sha256': digest(b'fixture')}
        lock = {**self.lock, 'native_components': {'sheenbidi': {'source_tree': 'a'*40}}}
        schema.native(value, lock)
        for nested in [component, component['artifacts']['libcrispembed.a']]:
            nested['debug_secret'] = 'SECRET-SENTINEL'
            with self.assertRaises(ValueError):
                schema.native(value, lock)
            del nested['debug_secret']

    def cli(self, script, extra=()):
        repo = Path(__file__).resolve().parents[2]
        return subprocess.run([sys.executable, str(repo / 'scripts' / script), '--project', str(self.project),
            '--staging', str(self.stage), *extra], capture_output=True, text=True, timeout=30)


    def test_recorder_cli_malformed_url_and_generated_at_never_echo_values(self):
        import shutil
        shutil.rmtree(self.stage / 'artifact')
        original = self.project / 'static/licenses.json'
        saved = original.read_bytes()
        signed = self.stage / 'signed.ipa'
        signed.write_bytes(b'unsigned local test fixture; validation must fail before signature checks')
        for field in ['link', 'generatedAt']:
            with self.subTest(field=field):
                value = json.loads(saved)
                if field == 'link':
                    value['licenses'] = [{'name': 'fixture', 'version': '1.0', 'license': 'MIT', 'author': 'Fixture',
                        'source': 'Backend', 'link': 'https://[SECRET-SENTINEL]/license'}]
                    value['counts'] = {'frontend': 0, 'backend': 1, 'total': 1}
                else:
                    value['generatedAt'] = 'SECRET-SENTINEL'
                self.write(original, value)
                result = self.cli('record_signed_mobile_ocr_draft.py',
                    ('--target', 'ios-arm64', '--artifact', str(signed)))
                self.assertEqual(result.returncode, 1)
                self.assertEqual(result.stderr, 'Mobile draft command failed [invalid-input]\n')
                self.assertNotIn('SECRET-SENTINEL', result.stdout + result.stderr)
                failure = self.stage / 'artifact/failure.json'
                self.assertEqual(json.loads(failure.read_text())['validation_status'], 'failed')
                self.assertNotIn('SECRET-SENTINEL', failure.read_text())
                shutil.rmtree(self.stage / 'artifact')

    def test_recorder_cli_parser_exception_never_echoes_json_input(self):
        # Invalid JSON's decode error can include raw malformed data in custom parsers;
        # the CLI boundary must suppress every exception traceback consistently.
        self.write(self.stage / 'preparation.json', '{"SECRET-SENTINEL":')
        result = self.cli('record_signed_mobile_ocr_draft.py',
            ('--target', 'ios-arm64', '--artifact', str(self.stage / 'signed.ipa')))
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stderr, 'Mobile draft command failed [invalid-record]\n')
        self.assertNotIn('SECRET-SENTINEL', result.stdout + result.stderr)

    def test_recorder_cli_argument_errors_never_echo_raw_values(self):
        for script in ['record_signed_mobile_ocr_draft.py']:
            result = self.cli(script, ('--SECRET-SENTINEL',))
            self.assertEqual(result.returncode, 2)
            self.assertEqual(result.stderr, 'Mobile draft command failed [invalid-arguments]\n')
            self.assertNotIn('SECRET-SENTINEL', result.stdout + result.stderr)


if __name__ == '__main__':
    unittest.main()
