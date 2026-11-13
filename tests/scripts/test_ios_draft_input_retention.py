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
import retain_ios_ocr_draft_inputs as retention
import record_signed_mobile_ocr_draft as recording
import ios_draft_retention_schema as schema


def digest(data):
    return hashlib.sha256(data).hexdigest()


class Retention(unittest.TestCase):
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
        for name in [retention.RUNTIME_LOCK, retention.PRODUCTION_LOCK]:
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
        commit = retention.git(self.project, 'rev-parse', 'HEAD')
        self.write(self.stage / 'preparation.json', {'schema': 'crispsorter-signed-ocr-draft-preparation-v1',
            'profile': retention.profile('ocr-draft'), 'qualification': 'Not established; build preparation only', 'target': 'ios-arm64', 'app_source': commit, 'version': '0.15.1',
            'production_source': self.lock['source_commit'], 'runtime_lock': retention.RUNTIME_LOCK,
            'runtime_lock_sha256': retention.sha(self.project / retention.RUNTIME_LOCK)})
        self.write(self.stage / 'layout.json', {'schema': 'crispsorter-mobile-release-layout-v1', 'app_source': commit,
            'app_tree': retention.git(self.project, 'rev-parse', 'HEAD^{tree}'), 'application_source_clean': True,
            'tracked_source_clean': True, 'generated_tracked_files': [], 'cargo_lock_sha256': retention.sha(self.project / 'Cargo.lock'),
            'cargo_manifest_sha256': retention.sha(self.project / 'src-tauri/Cargo.toml'),
            'dependency_sources': {'CrispEmbed': 'b'*40, 'CrispASR': 'd'*40, 'crisp-docx': 'e'*40},
            'checked_dependencies': ['crispembed'], 'mobile_qualification_lock_matches_release': True,
            'scope': 'Clean source layout and dependency identity only; no native build or behavior qualification'})
        notices = {name: {'source_repository': repo, 'source_path': path, 'git_blob': 'a'*40,
            'bytes': 7, 'sha256': digest(b'fixture')} for name, (repo, path) in retention.BASE_NOTICES.items()}
        self.write(self.stage / 'runtime/runtime-build.json', {'target': 'ios-arm64', **self.lock,
            'artifacts': {'libcrispembed.a': digest(b'fixture')}, 'native_provenance': {'schema_version': 1, 'components': {}, 'notices': notices}})
        self.write(self.stage / 'runtime/lib/libcrispembed.a', 'fixture')
        for name in notices:
            self.write(self.stage / 'runtime/notices' / name, 'fixture')
        self.ort = {'schema_version': 1, 'target': 'ios-arm64', 'kind': 'ocr-mobile-ort-runtime',
            'version': '1.23.0', 'api_version': 23, 'rust_binding': '2.0.0-rc.11',
            'details': {'framework_identifier': 'ios-arm64', 'architectures': ['arm64'], 'static': True, 'minimum_os_version': '15.1'},
            'recorded_at': '2026-11-13T00:00:00Z',
            'lock_sha256': retention.sha(self.project / 'scripts/ocr-mobile-ort-lock.json'),
            'archive': {key: self.pin[key] for key in ('url', 'filename', 'bytes', 'sha256')},
            'files': {name: {'bytes': 7, 'sha256': digest(b'fixture')} for name in retention.ORT_FILES}}
        self.write(self.stage / 'ort/ort-runtime.json', self.ort)
        for name in retention.ORT_FILES:
            self.write(self.stage / 'ort' / name, 'fixture')
        values = {'CRISPEMBED_SYS_LIB_DIR': str(self.stage / 'runtime/lib'), 'PROTOC': '/fixture/protoc',
                  'ORT_IOS_XCFWK_LOCATION': str(self.stage / 'ort/onnxruntime.xcframework')}
        self.write(self.project / '.cargo/config.toml', '\n\n' + retention.MARKER + '\n' + '\n'.join(
            f'[env.{key}]\nvalue = {json.dumps(value)}\nforce = true\n' for key, value in values.items()))
        self.write(self.stage / 'cargo-metadata.json', {'schema_version': 1, 'kind': 'locked-native-license-metadata', 'source_commit': commit,
            'cargo_version': 'cargo 1.91.0 (abcd1234 2026-01-01)', 'backend_count': 5,
            'metadata': {'packages': []}, 'metadata_sha256': digest(b'{"packages":[]}'),
            'target': 'aarch64-apple-ios', 'default_features': False, 'features': ['ocr-mobile', 'tauri/custom-protocol'],
            **{key: retention.sha(self.project / path) for key, path in [('cargo_lock_sha256', 'Cargo.lock'),
                ('workspace_manifest_sha256', 'Cargo.toml'), ('application_manifest_sha256', 'src-tauri/Cargo.toml')]}})
        self.write(self.stage / 'artifact/CrispSorter_0.15.1_ios-arm64-ocr-draft-unqualified.ipa', 'signed fixture')

    def write(self, path, value):
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(value) if isinstance(value, dict) else value)

    def collect(self):
        return retention.collect(self.project, self.stage)

    def fails(self):
        with self.assertRaises((ValueError, KeyError, TypeError, subprocess.CalledProcessError)):
            self.collect()
        manifest = json.loads((self.stage / 'replay-inputs/replay-inputs.json').read_text())
        self.assertEqual(manifest['input_retention_status'], 'incomplete')
        self.assertEqual(manifest['validation_status'], 'not_performed')
        self.assertNotIn('signed-draft.json', manifest['files'])
        return manifest

    def test_complete_allowlist_hashes_and_no_secrets_or_success_claim(self):
        for name in ['keychain-db', 'identity.p12', 'AuthKey.p8', 'profile.mobileprovision', 'credentials.json']:
            self.write(self.stage / name, 'SECRET-SENTINEL')
        result = self.collect()
        self.assertEqual(result['input_retention_status'], 'complete')
        self.assertEqual(result['signed_artifact']['validation_status'], 'not_asserted')
        self.assertEqual(result['signed_artifact']['sha256'], digest(b'signed fixture'))
        actual = {str(p.relative_to(self.stage / 'replay-inputs')) for p in (self.stage / 'replay-inputs').rglob('*') if p.is_file()}
        self.assertEqual(actual, set(result['files']) | {'replay-inputs.json'})
        self.assertEqual(result['total_bytes'], sum(x['bytes'] for x in result['files'].values()))
        for name, info in result['files'].items():
            path = self.stage / 'replay-inputs' / name
            self.assertEqual(retention.sha(path), info['sha256'])
            self.assertNotIn(b'SECRET-SENTINEL', path.read_bytes())

    def test_default_hidden_file_exclusion_keeps_every_retained_input(self):
        result = self.collect()
        files = result['files']
        self.assertIn('source/cargo-config.toml', files)
        self.assertEqual(files['source/cargo-config.toml']['original_source_path'], '.cargo/config.toml')
        stored = self.stage / 'replay-inputs/source/cargo-config.toml'
        self.assertEqual(stored.read_bytes(), (self.project / '.cargo/config.toml').read_bytes())
        # actions/upload-artifact v4 excludes dot components unless opted in.
        uploaded = {name for name in files if not any(part.startswith('.') for part in Path(name).parts)}
        self.assertEqual(uploaded, set(files))
        self.assertEqual(files['source/cargo-config.toml']['sha256'], retention.sha(self.project / '.cargo/config.toml'))

    def test_missing_original_archive_never_complete(self):
        (self.stage / 'runtime/lib/libcrispembed.a').unlink()
        self.fails()

    def test_native_hash_mismatch_never_complete(self):
        self.write(self.stage / 'runtime/lib/libcrispembed.a', 'wrong build')
        self.fails()

    def test_symlink_parent_is_rejected(self):
        directory = self.stage / 'runtime/lib'
        directory.rename(self.stage / 'elsewhere')
        directory.symlink_to(self.stage / 'elsewhere', target_is_directory=True)
        self.fails()

    def test_receipt_cannot_add_credential_path(self):
        self.ort['files']['../../AuthKey.p8'] = {'bytes': 7, 'sha256': digest(b'fixture')}
        self.write(self.stage / 'ort/ort-runtime.json', self.ort)
        self.fails()

    def test_config_with_unrelated_secret_comment_is_not_retained(self):
        with (self.project / '.cargo/config.toml').open('a') as stream:
            stream.write('# SECRET-SENTINEL\n')
        self.fails()
        self.assertFalse((self.stage / 'replay-inputs/source/cargo-config.toml').exists())

    def test_changed_lock_source_rejected(self):
        self.write(self.project / 'Cargo.lock', 'changed')
        self.fails()

    def test_missing_generated_record_rejected(self):
        (self.stage / 'cargo-metadata.json').unlink()
        self.fails()

    def test_mismatched_ort_binary_hash_rejected(self):
        self.write(self.stage / 'ort' / (retention.FRAMEWORK + 'onnxruntime'), 'changed')
        self.fails()

    def test_missing_signed_artifact_cannot_complete(self):
        next((self.stage / 'artifact').iterdir()).unlink()
        self.fails()

    def test_contradictory_distribution_controls_fail_closed(self):
        with patch.dict(os.environ, {'CRISPSORTER_RELEASE_PUBLISH': 'true'}):
            self.fails()

    def test_total_size_limit_and_no_overwrite(self):
        with patch.object(retention, 'MAX_TOTAL', 1):
            self.fails()
        with self.assertRaisesRegex(ValueError, 'replace'):
            self.collect()

    def test_unknown_fields_at_all_retained_record_levels_fail_before_copy(self):
        cases = [('preparation.json', []), ('preparation.json', ['profile']),
                 ('layout.json', []), ('layout.json', ['dependency_sources']),
                 ('runtime/runtime-build.json', []), ('runtime/runtime-build.json', ['native_provenance']),
                 ('runtime/runtime-build.json', ['native_provenance', 'notices', 'ggml-LICENSE.txt']),
                 ('ort/ort-runtime.json', []), ('ort/ort-runtime.json', ['archive']),
                 ('ort/ort-runtime.json', ['details']),
                 ('ort/ort-runtime.json', ['files', 'include/onnxruntime_c_api.h'])]
        for filename, nesting in cases:
            with self.subTest(filename=filename, nesting=nesting):
                path = self.stage / filename
                original = path.read_bytes()
                value = json.loads(original)
                target = value
                for key in nesting:
                    target = target[key]
                target['debug_secret'] = 'SECRET-SENTINEL'
                self.write(path, value)
                self.fails()
                for retained in (self.stage / 'replay-inputs').rglob('*'):
                    if retained.is_file():
                        self.assertNotIn(b'SECRET-SENTINEL', retained.read_bytes())
                import shutil
                shutil.rmtree(self.stage / 'replay-inputs')
                path.write_bytes(original)

    def test_generated_license_nested_unknown_field_rejected(self):
        path = self.project / 'static/licenses.json'
        value = json.loads(path.read_text())
        value['licenses'] = [{'name': 'fixture', 'version': '1.0', 'license': 'MIT', 'author': 'Fixture',
                              'source': 'Backend', 'link': 'https://example.invalid', 'debug_secret': 'SECRET-SENTINEL'}]
        value['counts'] = {'frontend': 0, 'backend': 1, 'total': 1}
        self.write(path, value)
        self.fails()
        self.assertFalse((self.stage / 'replay-inputs/source/static/licenses.json').exists())

    def test_duplicate_json_key_cannot_hide_discarded_secret(self):
        path = self.stage / 'preparation.json'
        path.write_text(path.read_text().replace('{', '{"version":"SECRET-SENTINEL",', 1))
        self.fails()
        self.assertFalse((self.stage / 'replay-inputs/staging/preparation.json').exists())

    def test_graph_top_level_and_nested_arbitrary_metadata_are_never_retained(self):
        path = self.stage / 'cargo-metadata.json'
        value = json.loads(path.read_text())
        value['debug_secret'] = 'SECRET-SENTINEL'
        value['metadata']['packages'] = [{'metadata': {'debug_secret': 'SECRET-SENTINEL'}}]
        value['metadata_sha256'] = digest(json.dumps(value['metadata'], separators=(',', ':')).encode())
        self.write(path, value)
        original_hash = retention.sha(path)
        result = self.collect()
        summary = json.loads((self.stage / 'replay-inputs/staging/cargo-binding-summary.json').read_text())
        self.assertFalse(summary['original_graph_retained'])
        self.assertEqual(summary['original_snapshot_sha256'], original_hash)
        self.assertNotIn('staging/cargo-metadata.json', result['files'])
        for retained in (self.stage / 'replay-inputs').rglob('*'):
            if retained.is_file():
                self.assertNotIn(b'SECRET-SENTINEL', retained.read_bytes())

    def test_graph_hash_tamper_is_rejected_without_raw_graph_copy(self):
        path = self.stage / 'cargo-metadata.json'
        value = json.loads(path.read_text())
        value['metadata']['debug_secret'] = 'SECRET-SENTINEL'
        self.write(path, value)
        self.fails()
        self.assertFalse((self.stage / 'replay-inputs/staging/cargo-binding-summary.json').exists())

    def test_summary_field_cannot_smuggle_arbitrary_text_or_nested_values(self):
        path = self.stage / 'cargo-metadata.json'
        value = json.loads(path.read_text())
        value['cargo_version'] = {'debug_secret': 'SECRET-SENTINEL'}
        self.write(path, value)
        self.fails()

    def test_config_unknown_nested_field_is_not_copied(self):
        path = self.project / '.cargo/config.toml'
        with path.open('a') as stream:
            stream.write('debug_secret = "SECRET-SENTINEL"\n')
        self.fails()
        self.assertFalse((self.stage / 'replay-inputs/source/cargo-config.toml').exists())

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
                    retention.profile('ocr-draft'), result, '0.15.1', self.stage / 'retained.ipa')
        record.assert_not_called()
        self.assertEqual({p.name for p in result.iterdir()}, {'failure.json'})
        self.assertNotIn('SECRET-SENTINEL', (result / 'failure.json').read_text())

    def test_failure_record_does_not_copy_exception_secret_text(self):
        result = self.stage / 'result'
        result.mkdir()
        with patch.object(recording, 'validate_inner', side_effect=ValueError('SECRET-SENTINEL')):
            with self.assertRaises(ValueError):
                recording.validate(self.project, self.stage, 'ios-arm64', self.stage / 'signed.ipa', {},
                    retention.profile('ocr-draft'), result, '0.15.1', self.stage / 'retained.ipa')
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

    def test_retention_cli_malformed_license_url_never_echoes_value(self):
        value = json.loads((self.project / 'static/licenses.json').read_text())
        value['licenses'] = [{'name': 'fixture', 'version': '1.0', 'license': 'MIT', 'author': 'Fixture',
            'source': 'Backend', 'link': 'https://[SECRET-SENTINEL]/license'}]
        value['counts'] = {'frontend': 0, 'backend': 1, 'total': 1}
        self.write(self.project / 'static/licenses.json', value)
        result = self.cli('retain_ios_ocr_draft_inputs.py')
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stderr, 'Mobile draft command failed [invalid-input]\n')
        self.assertNotIn('SECRET-SENTINEL', result.stdout + result.stderr)
        manifest = json.loads((self.stage / 'replay-inputs/replay-inputs.json').read_text())
        self.assertEqual(manifest['input_retention_status'], 'incomplete')

    def test_retention_cli_bad_generated_at_never_echoes_value(self):
        path = self.project / 'static/licenses.json'
        value = json.loads(path.read_text())
        value['generatedAt'] = 'SECRET-SENTINEL'
        self.write(path, value)
        result = self.cli('retain_ios_ocr_draft_inputs.py')
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stderr, 'Mobile draft command failed [invalid-input]\n')
        self.assertNotIn('SECRET-SENTINEL', result.stdout + result.stderr)

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

    def test_both_cli_argument_errors_never_echo_raw_values(self):
        for script in ['retain_ios_ocr_draft_inputs.py', 'record_signed_mobile_ocr_draft.py']:
            result = self.cli(script, ('--SECRET-SENTINEL',))
            self.assertEqual(result.returncode, 2)
            self.assertEqual(result.stderr, 'Mobile draft command failed [invalid-arguments]\n')
            self.assertNotIn('SECRET-SENTINEL', result.stdout + result.stderr)

    def test_workflow_failure_retention_and_no_concurrency_changes(self):
        repo = Path(__file__).resolve().parents[2]
        text = (repo / '.github/workflows/release.yml').read_text()
        segment = text[text.index('      - name: Verify final signed iOS OCR draft provenance'):text.index('  # ── macOS App Store')]
        self.assertIn('if: always() && env.CRISPSORTER_MOBILE_RELEASE_PROFILE', segment)
        self.assertIn('/signed-mobile-ocr-draft/replay-inputs', segment)
        self.assertNotIn('gh release', segment)
        self.assertNotIn('asc ', segment)


if __name__ == '__main__':
    unittest.main()
