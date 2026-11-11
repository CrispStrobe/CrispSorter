"""Failure and cleanup contracts for the live SMB fixture runner."""
import io
import subprocess
import threading
from types import SimpleNamespace
import unittest

from test_smb_source import read_fixture_ready, stop_fixture


class FixtureRunnerTests(unittest.TestCase):
    def test_accepts_only_a_valid_loopback_port(self):
        self.assertEqual(read_fixture_ready(SimpleNamespace(stdout=io.StringIO('{"port": 12345}\n'))), 12345)
        for payload in ['', 'not-json', '[]', '{}', '{"port": true}', '{"port": 0}', '{"port": 65536}', '{"port": "12345"}']:
            with self.subTest(payload=payload), self.assertRaises(RuntimeError):
                read_fixture_ready(SimpleNamespace(stdout=io.StringIO(payload)))

    def test_silent_startup_has_a_deadline(self):
        released = threading.Event()

        class SilentOutput:
            def readline(self):
                released.wait()
                return ''

        try:
            with self.assertRaisesRegex(RuntimeError, 'timed out'):
                read_fixture_ready(SimpleNamespace(stdout=SilentOutput()), timeout=0.01)
        finally:
            released.set()

    def test_cleanup_kills_an_unresponsive_fixture_and_closes_stdout(self):
        class Process:
            stdout = io.StringIO('')
            terminated = False
            killed = False
            waits = 0

            def poll(self):
                return None

            def terminate(self):
                self.terminated = True

            def kill(self):
                self.killed = True

            def wait(self, timeout):
                self.waits += 1
                if self.waits == 1:
                    raise subprocess.TimeoutExpired('fixture', timeout)
                return 0

        process = Process()
        stop_fixture(process)
        self.assertTrue(process.terminated)
        self.assertTrue(process.killed)
        self.assertEqual(process.waits, 2)
        self.assertTrue(process.stdout.closed)


if __name__ == '__main__':
    unittest.main()
