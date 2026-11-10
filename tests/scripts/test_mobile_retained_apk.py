"""Retained-artifact identity controls; no Android code is executed."""
import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
import warnings
import zipfile

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location('retained_apk', ROOT / 'tools/android-startup-probe/stage.py')
retained = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(retained)


class RetainedApk(unittest.TestCase):
    def fixture(self, root, *, receipt_change=None, duplicate=False):
        apk = b'APK identity fixture\0' * 100000
        pin = {'application_source': '1' * 40, 'application_tree': '2' * 40,
               'artifact_id': 7, 'artifact_name': 'test', 'run_id': 8,
               'apk_member': 'app/app.apk', 'build_receipt_member': 'app/build-receipt.json',
               'apk_bytes': len(apk), 'apk_sha256': hashlib.sha256(apk).hexdigest(),
               'package': 'com.crispstrobe.crispsorter', 'abi': 'x86_64'}
        receipt = {'sorter_commit': pin['application_source'], 'sorter_tree': pin['application_tree'],
                   'platform': 'android', 'package': pin['package'], 'application_source_clean': True,
                   'artifact': {'kind': 'apk', 'abis': ['x86_64'], 'bytes': len(apk), 'sha256': pin['apk_sha256']}}
        if receipt_change:
            receipt_change(receipt)
        data = json.dumps(receipt).encode()
        pin['build_receipt_sha256'] = hashlib.sha256(data).hexdigest()
        archive = root / 'artifact.zip'
        with zipfile.ZipFile(archive, 'w', compression=zipfile.ZIP_DEFLATED) as out:
            out.writestr(pin['apk_member'], apk)
            out.writestr(pin['build_receipt_member'], data)
            if duplicate:
                with warnings.catch_warnings():
                    warnings.simplefilter('ignore', UserWarning)
                    out.writestr(pin['apk_member'], apk)
        pin.update(artifact_bytes=archive.stat().st_size, artifact_sha256=retained.digest(archive))
        metadata = {'id': 7, 'name': 'test', 'size_in_bytes': pin['artifact_bytes'],
                    'digest': 'sha256:' + pin['artifact_sha256'],
                    'workflow_run': {'id': 8, 'head_sha': pin['application_source']}}
        return archive, metadata, pin

    def test_exact_input_is_streamed_without_a_startup_claim(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive, metadata, pin = self.fixture(root)
            result = retained.stage(archive, metadata, root / 'out', pin)
            self.assertFalse(result['application_rebuilt'])
            self.assertEqual(retained.digest(root / 'out/CrispSorter-ocr.apk'), pin['apk_sha256'])
            self.assertIn('not established', result['scope'])

    def test_changed_archive_or_source_metadata_is_rejected(self):
        for kind in ('archive', 'source'):
            with self.subTest(kind=kind), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                archive, metadata, pin = self.fixture(root)
                if kind == 'archive':
                    with archive.open('ab') as stream:
                        stream.write(b'changed')
                else:
                    metadata['workflow_run']['head_sha'] = '3' * 40
                with self.assertRaises(ValueError):
                    retained.stage(archive, metadata, root / 'out', pin)
                self.assertFalse((root / 'out').exists())

    def test_mismatched_application_receipt_never_stages_an_apk(self):
        changes = [lambda value: value.update(application_source_clean=False),
                   lambda value: value['artifact'].update(abis=['arm64-v8a']),
                   lambda value: value['artifact'].update(sha256='0' * 64)]
        for change in changes:
            with self.subTest(change=change), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                archive, metadata, pin = self.fixture(root, receipt_change=change)
                with self.assertRaises(ValueError):
                    retained.stage(archive, metadata, root / 'out', pin)
                self.assertFalse((root / 'out/CrispSorter-ocr.apk').exists())

    def test_ambiguous_duplicate_archive_members_are_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive, metadata, pin = self.fixture(root, duplicate=True)
            with self.assertRaisesRegex(ValueError, 'duplicate'):
                retained.stage(archive, metadata, root / 'out', pin)


if __name__ == '__main__':
    unittest.main()
