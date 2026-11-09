import hashlib
import importlib.util
import io
import json
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
    def test_quality_fixture_scope_keeps_small_language_and_layout_inputs(self):
        spec = probe.quality_spec()
        self.assertEqual([model['name'] for model in spec['models']], ['tesseract-deu'])
        self.assertEqual({case['language'] for case in spec['cases']}, {'eng', 'deu'})
        self.assertEqual({case['layout_engine'] for case in spec['cases']}, {None, 'cc'})
        german = next(case for case in spec['cases'] if case['language'] == 'deu')
        self.assertTrue({'Prüfung', 'Größe', 'Straße', 'München'}.issubset(german['required_tokens']))

    def test_quality_fixture_rejects_unsafe_names_and_relaxed_error_limits(self):
        for change in ({'filename': '../escape.png'}, {'max_unicode_cer': 1}, {'width': 100_000}):
            with self.subTest(change=change), tempfile.TemporaryDirectory() as directory:
                source = Path(directory) / 'quality.json'
                spec = probe.quality_spec()
                spec['cases'][0].update(change)
                source.write_text(json.dumps(spec))
                with self.assertRaises(ValueError):
                    probe.quality_spec(source)

    def document_manifest(self, root):
        data = b'%PDF-1.7\nfixture identity only; not a rendered document\n'
        (root / 'two-pages.pdf').write_bytes(data)
        manifest = {'schema_version': 1, 'filename': 'two-pages.pdf', 'bytes': len(data),
                    'sha256': hashlib.sha256(data).hexdigest(), 'pages': []}
        (root / 'manifest.json').write_text(json.dumps(manifest))
        return manifest

    def test_document_fixture_pin_is_checked_before_copying(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, output = root / 'source', root / 'output'
            source.mkdir()
            output.mkdir()
            manifest = self.document_manifest(source)
            result = probe.prepare_document_fixtures(output, source)
            self.assertTrue(result['available'])
            self.assertEqual(result['sha256'], manifest['sha256'])
            self.assertEqual((output / manifest['filename']).read_bytes(), (source / manifest['filename']).read_bytes())

    def test_document_fixture_mismatch_and_unsafe_path_are_not_copied(self):
        for change in ({'sha256': '0' * 64}, {'filename': '../escape.pdf'}):
            with self.subTest(change=change), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                source, output = root / 'source', root / 'output'
                source.mkdir()
                output.mkdir()
                manifest = self.document_manifest(source)
                manifest.update(change)
                (source / 'manifest.json').write_text(json.dumps(manifest))
                with self.assertRaises(ValueError):
                    probe.prepare_document_fixtures(output, source)
                self.assertEqual(list(output.iterdir()), [])

    def test_document_fixture_does_not_follow_destination_symlink(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, output = root / 'source', root / 'output'
            source.mkdir()
            output.mkdir()
            manifest = self.document_manifest(source)
            protected = root / 'original.pdf'
            protected.write_bytes(b'keep')
            (output / manifest['filename']).symlink_to(protected)
            with self.assertRaisesRegex(ValueError, 'Existing native document'):
                probe.prepare_document_fixtures(output, source)
            self.assertEqual(protected.read_bytes(), b'keep')

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
