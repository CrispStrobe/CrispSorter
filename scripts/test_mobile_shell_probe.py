"""Portable identity controls for the iOS UI runner; these do not run iOS."""
import argparse
import importlib.util
import json
import plistlib
import tempfile
import unittest
from unittest.mock import patch
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location('ios_shell_probe', ROOT / 'tools/ios-shell-probe/run.py')
PROBE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PROBE)
SOURCE = 'a' * 40


class CompiledInputIdentityTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.app = self.root / 'CrispSorter.app'
        self.app.mkdir()
        (self.app / 'Info.plist').write_bytes(plistlib.dumps({
            'CFBundleIdentifier': 'com.crispstrobe.crispsorter', 'DTPlatformName': 'iphonesimulator',
        }))
        # Deliberately not a runnable app: this suite verifies source/byte binding only.
        (self.app / 'identity-control').write_bytes(b'unchanged identity bytes')
        self.build = {
            'kind': 'ocr-mobile-app-build', 'platform': 'ios-simulator',
            'package': 'com.crispstrobe.crispsorter', 'sorter_commit': SOURCE,
            'application_source_clean': True,
            'artifact': PROBE.bundle_identity(self.app, 'iphonesimulator'),
        }
        self.receipt = self.root / 'build-receipt.json'
        self.receipt.write_text(json.dumps(self.build))
        self.args = argparse.Namespace(staged=None, app=self.app, build_receipt=self.receipt,
                                       output=self.root / 'evidence', application_source=SOURCE)

    def tearDown(self):
        self.temporary.cleanup()

    def test_stages_receipt_without_rewriting_application(self):
        before = self.build['artifact']
        output, app, pin = PROBE.prepare_inputs(self.args)
        self.assertEqual(app, self.app)
        self.assertEqual(pin['bundle_sha256'], before['sha256'])
        self.assertEqual(pin['application_source'], SOURCE)
        self.assertEqual(PROBE.bundle_identity(self.app, 'iphonesimulator'), before)
        self.assertEqual((output / 'build-receipt.json').read_bytes(), self.receipt.read_bytes())
        self.assertFalse(json.loads((output / 'input-receipt.json').read_text())['application_rebuilt_by_probe'])

    def test_rejects_a_different_or_dirty_application_source(self):
        self.args.application_source = 'b' * 40
        with self.assertRaisesRegex(ValueError, 'clean simulator application source'):
            PROBE.prepare_inputs(self.args)
        self.assertFalse(self.args.output.exists())
        self.args.application_source = SOURCE
        self.build['application_source_clean'] = False
        self.receipt.write_text(json.dumps(self.build))
        with self.assertRaisesRegex(ValueError, 'clean simulator application source'):
            PROBE.prepare_inputs(self.args)

    def test_rejects_mutated_bundle_before_ui_or_output_creation(self):
        (self.app / 'identity-control').write_bytes(b'changed identity bytes')
        with self.assertRaisesRegex(ValueError, 'differs from its build receipt'):
            PROBE.prepare_inputs(self.args)
        self.assertFalse(self.args.output.exists())

    def test_rejects_bundle_symlink(self):
        alias = self.root / 'Alias.app'
        alias.symlink_to(self.app, target_is_directory=True)
        self.args.app = alias
        with self.assertRaisesRegex(ValueError, 'actual .app directory'):
            PROBE.prepare_inputs(self.args)

    def test_preserves_existing_evidence(self):
        self.args.output.mkdir()
        prior = self.args.output / 'navigation.json'
        prior.write_bytes(b'prior result')
        with self.assertRaisesRegex(ValueError, 'new or empty'):
            PROBE.prepare_inputs(self.args)
        self.assertEqual(prior.read_bytes(), b'prior result')

    def test_recovers_partial_runner_report_without_overwriting_it(self):
        data = self.root / 'runner'
        evidence = data / 'Documents/ocr-shell-probe'
        evidence.mkdir(parents=True)
        partial = b'{"status":"failed","snapshots":[{"name":"inbox"}]}'
        (evidence / 'navigation.json').write_bytes(partial)
        self.args.output.mkdir()
        with patch.object(PROBE, 'output', return_value=str(data)) as resolve:
            self.assertTrue(PROBE.collect_runner_evidence('simulator', self.args.output))
            self.assertEqual((self.args.output / 'evidence/navigation.json').read_bytes(), partial)
            resolve.assert_called_once()
            self.assertEqual(resolve.call_args.kwargs['timeout'], 30)
        (evidence / 'navigation.json').write_bytes(b'new unrelated runner result')
        with patch.object(PROBE, 'output', side_effect=AssertionError('Do not reread existing evidence')):
            self.assertFalse(PROBE.collect_runner_evidence('simulator', self.args.output))
        self.assertEqual((self.args.output / 'evidence/navigation.json').read_bytes(), partial)

    def test_recovery_rejects_symlinks_and_excessive_members(self):
        data = self.root / 'runner'
        evidence = data / 'Documents/ocr-shell-probe'
        evidence.mkdir(parents=True)
        self.args.output.mkdir()
        link = evidence / 'escape'
        link.symlink_to(self.receipt)
        with patch.object(PROBE, 'output', return_value=str(data)):
            with self.assertRaisesRegex(RuntimeError, 'size or member type'):
                PROBE.collect_runner_evidence('simulator', self.args.output)
        self.assertFalse((self.args.output / 'evidence').exists())
        link.unlink()
        for index in range(41):
            (evidence / f'part-{index}').write_bytes(b'')
        with patch.object(PROBE, 'output', return_value=str(data)):
            with self.assertRaisesRegex(RuntimeError, 'size or member type'):
                PROBE.collect_runner_evidence('simulator', self.args.output)
        self.assertFalse((self.args.output / 'evidence').exists())


if __name__ == '__main__':
    unittest.main()
