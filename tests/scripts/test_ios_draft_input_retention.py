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
        self.write(self.project / '.gitignore', '.cargo/\n')
        for args in [('init', '-q'), ('add', '.'), ('-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', 'commit', '-qm', 'fixture')]:
            subprocess.run(['git', '-C', str(self.project), *args], check=True, capture_output=True)
        commit = retention.git(self.project, 'rev-parse', 'HEAD')
        self.write(self.stage / 'preparation.json', {'target': 'ios-arm64', 'app_source': commit, 'version': '0.15.1',
            'production_source': self.lock['source_commit'], 'runtime_lock': retention.RUNTIME_LOCK,
            'runtime_lock_sha256': retention.sha(self.project / retention.RUNTIME_LOCK)})
        self.write(self.stage / 'layout.json', {'app_source': commit, 'app_tree': retention.git(self.project, 'rev-parse', 'HEAD^{tree}')})
        notices = {name: {'bytes': 7, 'sha256': digest(b'fixture')} for name in retention.BASE_NOTICES}
        self.write(self.stage / 'runtime/runtime-build.json', {'target': 'ios-arm64', **self.lock,
            'artifacts': {'libcrispembed.a': digest(b'fixture')}, 'native_provenance': {'notices': notices}})
        self.write(self.stage / 'runtime/lib/libcrispembed.a', 'fixture')
        for name in notices:
            self.write(self.stage / 'runtime/notices' / name, 'fixture')
        self.ort = {'target': 'ios-arm64', 'kind': 'ocr-mobile-ort-runtime',
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
        self.write(self.stage / 'cargo-metadata.json', {'kind': 'locked-native-license-metadata', 'source_commit': commit,
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
        with self.assertRaises((ValueError, KeyError, TypeError)):
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
        self.assertFalse((self.stage / 'replay-inputs/source/.cargo/config.toml').exists())

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
