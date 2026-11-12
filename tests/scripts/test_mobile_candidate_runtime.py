import hashlib
import importlib.util
import json
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import unittest
import warnings
import zipfile
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'scripts'))
import prepare_ocr_mobile_candidate_runtime as candidate
import ocr_mobile_build_receipt as receipt


class CandidateRuntimeTests(unittest.TestCase):
    def archive(self, root, members):
        path = root / 'candidate.zip'
        with warnings.catch_warnings():
            warnings.simplefilter('ignore', UserWarning)
            with zipfile.ZipFile(path, 'w') as archive:
                for name, data, mode in members:
                    info = zipfile.ZipInfo(name)
                    info.external_attr = mode << 16
                    archive.writestr(info, data)
        pin = {'archive_sha256': candidate.digest(path),
               'files': {name: {'bytes': len(data), 'sha256': hashlib.sha256(data).hexdigest()}
                         for name, data, _ in members}}
        return path, pin

    def test_valid_archive_is_staged_without_rebuilding_or_executing(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive, pin = self.archive(root, [('crispembed-mobile/lib/libcrispembed.so', b'library', stat.S_IFREG),
                                               ('ocr-link-probe', b'probe', stat.S_IFREG)])
            candidate.unpack(archive, root / 'output', pin)
            self.assertEqual((root / 'output/lib/libcrispembed.so').read_bytes(), b'library')
            self.assertEqual((root / 'output/ocr-link-probe').read_bytes(), b'probe')
            candidate.verify_staged(root / 'output', pin)
            (root / 'output/lib/libcrispembed.so').write_bytes(b'changed')
            with self.assertRaisesRegex(ValueError, 'Staged candidate differs'):
                candidate.verify_staged(root / 'output', pin)
            with self.assertRaisesRegex(ValueError, 'must not exist'):
                candidate.unpack(archive, root / 'output', pin)

    def test_untrusted_members_are_rejected_before_any_output_is_written(self):
        for members in [
            [('crispembed-mobile/lib/good', b'good', stat.S_IFREG), ('../escape', b'bad', stat.S_IFREG)],
            [('/absolute', b'bad', stat.S_IFREG)],
            [('symlink', b'target', stat.S_IFLNK)],
            [('device', b'device', stat.S_IFCHR)],
            [('duplicate', b'first', stat.S_IFREG), ('duplicate', b'second', stat.S_IFREG)],
        ]:
            with self.subTest(members=members), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                archive, pin = self.archive(root, members)
                with self.assertRaises(ValueError):
                    candidate.unpack(archive, root / 'output', pin)
                self.assertFalse((root / 'output').exists())

    def test_archive_member_hash_size_and_inventory_are_independently_bound(self):
        for field in ('archive_sha256', 'sha256', 'bytes', 'inventory'):
            with self.subTest(field=field), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                archive, pin = self.archive(root, [('notice', b'notice', stat.S_IFREG)])
                if field == 'archive_sha256':
                    pin[field] = '0' * 64
                elif field == 'inventory':
                    pin['files']['missing'] = pin['files']['notice']
                else:
                    pin['files']['notice'][field] = '0' * 64 if field == 'sha256' else 999
                with self.assertRaises(ValueError):
                    candidate.unpack(archive, root / 'output', pin)
                self.assertFalse((root / 'output').exists())

    def test_excessive_members_are_rejected_before_read_or_write(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive, pin = self.archive(root, [('huge', b'notice', stat.S_IFREG)])
            with patch.object(zipfile.ZipFile, 'infolist') as infos:
                info = zipfile.ZipInfo('huge')
                info.file_size = 513 * 1024 * 1024
                infos.return_value = [info]
                with self.assertRaisesRegex(ValueError, 'total size'):
                    candidate.unpack(archive, root / 'output', pin)
            self.assertFalse((root / 'output').exists())

    def test_receipt_defaults_to_production_lock_and_accepts_only_committed_explicit_lock(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'scripts').mkdir()
            normal = {'source_commit': 'a' * 40, 'ggml_commit': 'b' * 40}
            proposed = {'source_commit': 'c' * 40, 'ggml_commit': 'b' * 40}
            for name, value in [('ocr-mobile-runtime-lock.json', normal), ('candidate.json', proposed)]:
                (root / 'scripts' / name).write_text(json.dumps(value))
            for args in (['init', '-q'], ['add', '.'],
                         ['-c', 'user.name=Test', '-c', 'user.email=test@example.invalid', 'commit', '-qm', 'locks']):
                subprocess.run(['git', '-C', str(root), *args], check=True, capture_output=True)
            self.assertEqual(receipt.runtime_lock_identity(root)[0], normal)
            lock, identity = receipt.runtime_lock_identity(root, 'scripts/candidate.json')
            self.assertEqual(lock, proposed)
            self.assertEqual(identity, {'path': 'scripts/candidate.json',
                                       'sha256': receipt.digest(root / 'scripts/candidate.json')})
            (root / 'scripts/candidate.json').write_text(json.dumps(normal))
            with self.assertRaisesRegex(ValueError, 'committed bytes'):
                receipt.runtime_lock_identity(root, 'scripts/candidate.json')
            for path in ('../outside.json', str(root / 'scripts/ocr-mobile-runtime-lock.json')):
                with self.assertRaises(ValueError):
                    receipt.runtime_lock_identity(root, path)

    def test_candidate_source_is_verified_before_archive_staging(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'source').mkdir()
            (root / 'lock.json').write_text('{}')
            with patch.object(candidate, 'committed_pin', return_value={'runtime_lock': 'lock.json'}), \
                 patch.object(candidate, 'select_lock', return_value={'lock': 'lock.json'}), \
                 patch.object(candidate, 'check_source', side_effect=ValueError('source mismatch')), \
                 patch.object(candidate, 'unpack') as unpack:
                with self.assertRaisesRegex(ValueError, 'source mismatch'):
                    candidate.prepare(root / 'archive', root / 'output', root / 'source', root)
                unpack.assert_not_called()

    def test_receipt_rejects_candidate_with_default_lock_and_binds_explicit_lock(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            runtime = {'source_commit': 'c' * 40, 'ggml_commit': 'b' * 40, 'target': 'android-x86_64'}
            runtime_path = root / 'runtime-build.json'
            runtime_path.write_text(json.dumps(runtime))
            artifact, ort = root / 'app.apk', root / 'ort-runtime.json'
            with patch.object(receipt, 'source_tree_identity', return_value={}), \
                 patch.object(receipt, 'git', return_value='a' * 40), \
                 patch.object(receipt, 'runtime_lock_identity') as selected, \
                 patch.object(receipt, 'android_identity', return_value={}) as identity, \
                 patch.object(receipt, 'ort_identity', return_value={}), \
                 patch.object(receipt, 'verify_native_provenance', return_value={}) as provenance:
                selected.return_value = ({'source_commit': 'a' * 40, 'ggml_commit': 'b' * 40}, {})
                with self.assertRaisesRegex(ValueError, 'does not match this application source lock'):
                    receipt.record(root, 'android', artifact, runtime_path, ort)
                identity.assert_not_called()
                lock = {'source_commit': 'c' * 40, 'ggml_commit': 'b' * 40}
                lock_identity = {'path': 'scripts/candidate.json', 'sha256': 'd' * 64}
                selected.return_value = (lock, lock_identity)
                value = receipt.record(root, 'android', artifact, runtime_path, ort, 'scripts/candidate.json')
                selected.assert_called_with(root, 'scripts/candidate.json')
                provenance.assert_called_with(artifact, 'android', root, runtime, lock)
                self.assertEqual(value['runtime_lock'], lock_identity)
                self.assertEqual(value['runtime_receipt_sha256'], receipt.digest(runtime_path))

    def test_workflow_requires_explicit_dispatch_and_single_x86_target(self):
        workflow = (ROOT / '.github/workflows/ocr-mobile-native.yml').read_text()
        self.assertIn('candidate_runtime:', workflow)
        self.assertIn('type: boolean\n        default: false', workflow)
        self.assertIn('"$CANDIDATE_RUNTIME" == true && "$REQUESTED_TARGET" != android-x86_64', workflow)
        self.assertIn("if: github.event_name != 'workflow_dispatch' || !inputs.candidate_runtime", workflow)
        self.assertEqual(workflow.count("if: github.event_name == 'workflow_dispatch' && inputs.candidate_runtime"), 2)
        self.assertIn('actions/artifacts/13985650215/zip', workflow)
        self.assertIn("--runtime-lock '${{ steps.runtime.outputs.lock }}'", workflow)
        self.assertIn('ocr-mobile-runtime/candidate-reuse.json', workflow)


if __name__ == '__main__':
    unittest.main()
