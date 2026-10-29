#!/usr/bin/env python3
"""Exercise the packaged OCR worker smoke with a subprocess protocol stub."""
import os
from pathlib import Path
import sys
import tempfile
import unittest

from verify_release_artifacts import check_ocr_worker


class WorkerSmoke(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.stub = self.root / 'worker_stub.py'
        self.stub.write_text('''import os, pathlib, sys
if sys.argv[1] != "--crispsorter-ocr-worker":
    sys.exit(2)
if len(sys.argv) != 4:
    print("OCR worker requires request and response paths", file=sys.stderr)
    sys.exit(1)
if os.environ.get("CREATE_RESPONSE"):
    pathlib.Path(sys.argv[3]).write_text("bad")
if os.environ.get("ACCEPT_INVALID"):
    sys.exit(0)
if pathlib.Path(sys.argv[2]).read_text() != "not json":
    sys.exit(2)
print("parse OCR worker request: invalid JSON", file=sys.stderr)
sys.exit(1)
''')
        self.command = [sys.executable, str(self.stub)]
        self.env = os.environ.copy()
        self.env.pop('CREATE_RESPONSE', None)
        self.env.pop('ACCEPT_INVALID', None)

    def test_expected_controlled_failures_pass(self):
        result = check_ocr_worker(self.command, self.root, self.env)
        self.assertEqual(result['exit_codes'], [1, 1])

    def test_success_on_invalid_request_fails(self):
        self.env['ACCEPT_INVALID'] = '1'
        with self.assertRaisesRegex(ValueError, 'malformed input'):
            check_ocr_worker(self.command, self.root, self.env)

    def test_unexpected_response_fails(self):
        self.env['CREATE_RESPONSE'] = '1'
        with self.assertRaisesRegex(ValueError, 'without a response'):
            check_ocr_worker(self.command, self.root, self.env)


if __name__ == '__main__':
    unittest.main()
