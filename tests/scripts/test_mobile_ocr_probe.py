import hashlib
import importlib.util
import io
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location('mobile_probe_inputs', ROOT / 'tools/mobile-ocr-probe/prepare.py')
probe = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(probe)


class Response(io.BytesIO):
    def __init__(self, body, url='https://example.invalid/model.gguf'):
        super().__init__(body)
        self.url = url

    def geturl(self):
        return self.url


class FixtureTests(unittest.TestCase):
    def model(self, body=b'GGUF-synthetic'):
        return {'filename': 'fixture.gguf', 'url': 'https://example.invalid/model.gguf',
                'sha256': hashlib.sha256(body).hexdigest()}

    def test_valid_pin_commits_and_reuses_without_network(self):
        body = b'GGUF-synthetic'
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with patch.object(probe, 'urlopen', return_value=Response(body)) as fetch:
                path = probe.fetch_model(self.model(body), root)
                self.assertEqual(path.read_bytes(), body)
                self.assertEqual(probe.fetch_model(self.model(body), root), path)
                self.assertEqual(fetch.call_count, 1)
            self.assertEqual(list(root.iterdir()), [path])

    def test_hash_failure_removes_partial_fixture(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with patch.object(probe, 'urlopen', return_value=Response(b'corrupted')):
                with self.assertRaisesRegex(ValueError, 'SHA-256 mismatch'):
                    probe.fetch_model(self.model(), root)
            self.assertEqual(list(root.iterdir()), [])

    def test_truncated_or_oversized_response_never_becomes_ready(self):
        for body, cap in ((b'', 64), (b'GGUF-too-large', 4)):
            with self.subTest(body=body), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                with patch.object(probe, 'MAX_MODEL_BYTES', cap), patch.object(probe, 'urlopen', return_value=Response(body)):
                    with self.assertRaises(ValueError):
                        probe.fetch_model(self.model(body), root)
                self.assertEqual(list(root.iterdir()), [])

    def test_wrong_existing_fixture_is_preserved_and_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / 'fixture.gguf'
            path.write_bytes(b'keep-existing')
            with patch.object(probe, 'urlopen') as fetch:
                with self.assertRaisesRegex(ValueError, 'Existing model'):
                    probe.fetch_model(self.model(), root)
                fetch.assert_not_called()
            self.assertEqual(path.read_bytes(), b'keep-existing')

    def test_invalid_path_or_insecure_redirect_does_not_write_model(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            bad = self.model()
            bad['filename'] = '../escape.gguf'
            with patch.object(probe, 'urlopen') as fetch:
                with self.assertRaisesRegex(ValueError, 'Invalid model'):
                    probe.fetch_model(bad, root)
                fetch.assert_not_called()
            with patch.object(probe, 'urlopen', return_value=Response(b'GGUF-synthetic', 'http://example.invalid/model')):
                with self.assertRaisesRegex(ValueError, 'outside HTTPS'):
                    probe.fetch_model(self.model(), root)
            self.assertEqual(list(root.iterdir()), [])


if __name__ == '__main__':
    unittest.main()
