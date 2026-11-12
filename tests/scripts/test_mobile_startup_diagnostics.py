"""Failure-retention controls; no emulator or application is executed here."""
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'scripts'))
SPEC = importlib.util.spec_from_file_location('startup', ROOT / 'scripts/mobile_simulator_smoke.py')
startup = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(startup)


class StartupEvidence(unittest.TestCase):
    def test_launcher_resolution_requires_one_activity_in_the_installed_package(self):
        adb = ['adb', '-s', 'emulator-5554']
        with patch.object(startup, 'run', return_value=(
                'priority=0 preferredOrder=0 match=0x108000 specificIndex=-1 isDefault=false\n'
                'com.crispstrobe.crispsorter/.MainActivity')) as run:
            self.assertEqual(startup.android_launcher(adb), 'com.crispstrobe.crispsorter/.MainActivity')
            self.assertIn('android.intent.category.LAUNCHER', run.call_args.args[0])
        for output in ('No activity found', 'other.app/.MainActivity',
                       'com.crispstrobe.crispsorter/.One\ncom.crispstrobe.crispsorter/.Two'):
            with self.subTest(output=output), patch.object(startup, 'run', return_value=output):
                with self.assertRaises(RuntimeError):
                    startup.android_launcher(adb)

    def test_failed_command_keeps_exit_output_and_timeout_bytes(self):
        command = ['adb', '-s', 'emulator-5554', 'shell', 'am', 'start']
        error = subprocess.CalledProcessError(1, command, output='Error: activity unavailable', stderr='details')
        result = startup.command_failure(error)
        self.assertEqual(result['command'], command)
        self.assertEqual(result['returncode'], 1)
        self.assertEqual(result['stdout'], 'Error: activity unavailable')
        self.assertEqual(result['stderr'], 'details')
        timeout = subprocess.TimeoutExpired(command, 3, output=b'partial\xff', stderr=b'still starting')
        result = startup.command_failure(timeout)
        self.assertEqual(result['timeout_seconds'], 3)
        self.assertEqual(result['stderr'], 'still starting')
        self.assertIn('partial', result['stdout'])

    def test_diagnostics_never_touch_a_physical_or_unverified_target(self):
        with tempfile.TemporaryDirectory() as directory, patch.object(startup.subprocess, 'run') as run:
            self.assertIn('skipped', startup.android_failure_evidence('physical-device', Path(directory)))
            run.assert_not_called()
            run.return_value = subprocess.CompletedProcess([], 0, stdout='0\n', stderr='')
            self.assertIn('skipped', startup.android_failure_evidence('emulator-5554', Path(directory)))
            self.assertEqual(run.call_count, 1)
            self.assertEqual(list(Path(directory).iterdir()), [])

    def test_nonzero_diagnostics_and_large_logs_remain_bounded_evidence(self):
        def run(command, **kwargs):
            if 'ro.kernel.qemu' in command:
                return subprocess.CompletedProcess(command, 0, stdout='1\n', stderr='')
            if 'screencap' in command:
                return subprocess.CompletedProcess(command, 0, stdout=b'\x89PNG\r\n\x1a\nfixture', stderr=b'')
            return subprocess.CompletedProcess(command, 1, stdout='X' * 100, stderr='diagnostic command failed')
        with tempfile.TemporaryDirectory() as directory, patch.object(startup.subprocess, 'run', side_effect=run), \
                patch.object(startup, 'MAX_DIAGNOSTIC_TEXT', 16):
            root = Path(directory)
            evidence = startup.android_failure_evidence('emulator-5554', root)
            self.assertEqual(len(evidence['commands']), 5)
            self.assertTrue(all(item['returncode'] == 1 and item['output_truncated'] for item in evidence['commands']))
            self.assertIn('diagnostic command failed', (root / 'launcher-resolution.txt').read_text())
            self.assertNotIn('X' * 17, (root / 'logcat.txt').read_text())
            self.assertTrue((root / 'failure-screen.png').is_file())

    def test_expired_collection_budget_does_not_start_more_commands(self):
        verified = subprocess.CompletedProcess([], 0, stdout='1\n', stderr='')
        with tempfile.TemporaryDirectory() as directory, patch.object(startup.subprocess, 'run', return_value=verified) as run, \
                patch.object(startup.time, 'monotonic', side_effect=[0, 31, 31]):
            result = startup.android_failure_evidence('emulator-5554', Path(directory))
            self.assertTrue(result['budget_exhausted'])
            self.assertEqual(run.call_count, 1)

    def test_main_failure_stays_fatal_when_diagnostic_collection_fails(self):
        failure = subprocess.CalledProcessError(7, ['adb', 'shell', 'am', 'start'], output='original stdout', stderr='original stderr')
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / 'evidence'
            argv = ['mobile_simulator_smoke.py', '--platform', 'android', '--target', 'emulator-5554',
                    '--artifact', str(Path(directory) / 'app.apk'), '--output', str(target)]
            with patch.object(sys, 'argv', argv), patch.object(startup, 'android', side_effect=failure), \
                    patch.object(startup, 'android_failure_evidence', side_effect=RuntimeError('capture unavailable')):
                with self.assertRaises(subprocess.CalledProcessError) as raised:
                    startup.main()
            self.assertIs(raised.exception, failure)
            receipt = json.loads((target / 'receipt.json').read_text())
            self.assertEqual(receipt['status'], 'failed')
            self.assertEqual(receipt['failed_command']['returncode'], 7)
            self.assertEqual(receipt['failed_command']['stdout'], 'original stdout')
            self.assertEqual(receipt['failed_command']['stderr'], 'original stderr')
            self.assertEqual(receipt['failure_evidence_error'], 'capture unavailable')


if __name__ == '__main__':
    unittest.main()
