import hashlib
import json
from pathlib import Path
import tempfile
import unittest
from verify_crispembed_bundle import verify


class RuntimeLockTest(unittest.TestCase):
    def test_modified_and_unlisted_bundles_are_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            bundle = root / 'runtime.zip'
            bundle.write_bytes(b'reviewed runtime bytes')
            lock = root / 'lock.json'
            lock.write_text(json.dumps({'version': 'test', 'source_commit': 'abc', 'assets': {
                'runtime.zip': {'bytes': bundle.stat().st_size,
                                'sha256': hashlib.sha256(bundle.read_bytes()).hexdigest()}}}))
            self.assertEqual(verify(bundle, 'runtime.zip', lock)['source_commit'], 'abc')
            with self.assertRaises(ValueError):
                verify(bundle, 'other-runtime.zip', lock)
            bundle.write_bytes(b'unreviewed replacement')
            with self.assertRaises(ValueError):
                verify(bundle, 'runtime.zip', lock)


if __name__ == '__main__':
    unittest.main()
