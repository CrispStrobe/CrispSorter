"""Source/package/qualification controls; no models, builds or consent."""
import copy
import hashlib
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch
import subprocess

import release_ocr_qualification as qualification
import verify_release_artifacts as verifier

ROOT = Path(__file__).resolve().parents[1]
SOURCE = '1' * 40
FIXTURE = 'f' * 64


class QualificationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / 'src-tauri').mkdir()
        (self.root / 'src-tauri/tauri.conf.json').write_text('{"version":"0.15.1"}')
        self.package = self.root / 'target/release/bundle/deb/CrispSorter_0.15.1_amd64.deb'
        self.package.parent.mkdir(parents=True)
        self.package.write_bytes(b'nonexecutable package fixture')
        self.receipt = {'schema': 'crispsorter-release-receipt-v1', 'platform': 'linux-x86_64',
                        'app_source': SOURCE, 'version': '0.15.1',
                        'ocr_qualification': qualification.package_control(),
                        'artifacts': [{'filename': self.package.name, 'bytes': self.package.stat().st_size,
                                       'sha256': qualification.sha(self.package)}]}
        self.native = {'models': [{'registry_name': name, 'bytes': 1, 'sha256': 'a' * 64}
                                  for name in sorted(qualification.MODELS)],
                       'cases': [{'preset': name, 'matched': True, 'text': 'Cobalt Warranty 2031'}
                                 for name in sorted(qualification.PRESETS)]}
        self.native.update(schema='crispsorter-native-ocr-presets-v1', app_source=SOURCE,
                           fixture={'filename': 'ocr-task.png', 'sha256': FIXTURE},
                           package_receipt={'sha256': 'b' * 64, 'package': copy.deepcopy(self.receipt['artifacts'][0])})

    def test_package_success_explicitly_excludes_licensed_qualification(self):
        control = qualification.package_control()
        self.assertEqual(control['permissive_package_control']['status'], 'passed')
        self.assertEqual(control['licensed_native_presets']['status'], 'not_run')
        self.assertEqual(control['licensed_native_presets']['qualification'], 'not established')
        self.assertTrue(control['licensed_native_presets']['required_before_publication'])

    def test_successful_native_smoke_seals_distinct_source_package_and_receipt(self):
        value = qualification.seal(self.receipt, 'b' * 64, self.native, SOURCE, '0.15.1', FIXTURE)
        qualification.validate(value, self.receipt, 'b' * 64, SOURCE, '0.15.1', FIXTURE)
        self.assertEqual(value['package'], self.receipt['artifacts'][0])
        self.assertIn('native app-binding', value['scope'])
        self.assertEqual(self.receipt['ocr_qualification']['licensed_native_presets']['status'], 'not_run')

    def test_missing_duplicate_failed_and_invalid_licensed_evidence_fail_closed(self):
        for case in ['empty', 'missing-qwen', 'duplicate-model', 'bad-model-hash', 'missing-preset',
                     'duplicate-preset', 'failed', 'missing-text', 'unscored']:
            value = copy.deepcopy(self.native)
            if case == 'empty': value = {}
            elif case == 'missing-qwen': value['models'] = value['models'][:-1]
            elif case == 'duplicate-model': value['models'][0] = value['models'][1]
            elif case == 'bad-model-hash': value['models'][0]['sha256'] = 'invalid'
            elif case == 'missing-preset': value['cases'] = value['cases'][:-1]
            elif case == 'duplicate-preset': value['cases'][0] = value['cases'][1]
            elif case == 'failed': value['cases'][0]['matched'] = False
            elif case == 'missing-text': value['cases'][0]['text'] = 'Cobalt'
            else: del value['cases'][0]['matched']
            with self.subTest(case=case), self.assertRaises(ValueError):
                qualification.seal(self.receipt, 'b' * 64, value, SOURCE, '0.15.1', FIXTURE)

    def test_source_version_package_and_receipt_mismatches_are_rejected(self):
        good = qualification.seal(self.receipt, 'b' * 64, self.native, SOURCE, '0.15.1', FIXTURE)
        for case in ['source', 'version', 'package', 'receipt', 'skip']:
            value = copy.deepcopy(good)
            if case == 'source': value['app_source'] = '2' * 40
            elif case == 'version': value['version'] = '0.15.0'
            elif case == 'package': value['package']['sha256'] = 'c' * 64
            elif case == 'receipt': value['package_receipt_sha256'] = 'c' * 64
            else: value['status'] = 'not_run'
            with self.subTest(case=case), self.assertRaises(ValueError):
                qualification.validate(value, self.receipt, 'b' * 64, SOURCE, '0.15.1', FIXTURE)

    def test_raw_native_report_cannot_be_resealed_for_another_valid_source_package_or_fixture(self):
        changed = copy.deepcopy(self.receipt)
        changed['app_source'] = '2' * 40
        changed['artifacts'][0]['sha256'] = 'c' * 64
        qualification.package_entry(changed, '2' * 40, '0.15.1')
        with self.assertRaisesRegex(ValueError, 'Raw native evidence'):
            qualification.seal(changed, 'd' * 64, self.native, '2' * 40, '0.15.1', FIXTURE)
        with self.assertRaisesRegex(ValueError, 'Raw native evidence'):
            qualification.seal(self.receipt, 'b' * 64, self.native, SOURCE, '0.15.1', 'e' * 64)
        with self.assertRaisesRegex(ValueError, 'Raw native evidence'):
            qualification.seal(self.receipt, 'd' * 64, self.native, SOURCE, '0.15.1', FIXTURE)
        fresh = copy.deepcopy(self.native)
        fresh['app_source'] = '2' * 40
        fresh['package_receipt'] = {'sha256': 'd' * 64, 'package': changed['artifacts'][0]}
        value = qualification.seal(changed, 'd' * 64, fresh, '2' * 40, '0.15.1', FIXTURE)
        qualification.validate(value, changed, 'd' * 64, '2' * 40, '0.15.1', FIXTURE)

    def run_sealer(self):
        receipt, native, output = [self.root / name for name in ['package.json', 'native.json', 'qualified.json']]
        receipt.write_text(json.dumps(self.receipt))
        report = copy.deepcopy(self.native)
        report['package_receipt']['sha256'] = qualification.sha(receipt)
        native.write_text(json.dumps(report))
        with patch.object(Path, 'cwd', return_value=self.root), \
                patch.object(qualification.subprocess, 'check_output', return_value=SOURCE), \
                patch.object(qualification, 'fixture_sha256', return_value=FIXTURE), \
                patch.object(sys, 'argv', ['seal', '--receipt', str(receipt), '--native-smoke', str(native), '--output', str(output)]):
            qualification.main()
        return output

    def test_sealer_checks_actual_package_bytes_before_writing_success(self):
        output = self.run_sealer()
        value = json.loads(output.read_text())
        self.assertEqual(value['package_receipt_sha256'], qualification.sha(self.root / 'package.json'))
        output.unlink()
        self.package.write_bytes(b'changed package bytes')
        with self.assertRaisesRegex(ValueError, 'changed after'):
            self.run_sealer()
        self.assertFalse(output.exists())

    def test_sealer_rejects_multiple_and_symlinked_package_candidates(self):
        other = self.root / 'target/other/bundle/deb' / self.package.name
        other.parent.mkdir(parents=True)
        other.write_bytes(self.package.read_bytes())
        with self.assertRaisesRegex(ValueError, 'exactly one'):
            self.run_sealer()
        other.unlink()
        other.symlink_to(self.package)
        self.package.unlink()
        with self.assertRaisesRegex(ValueError, 'exactly one'):
            self.run_sealer()
        self.assertFalse((self.root / 'qualified.json').exists())

    def test_package_receipt_is_written_only_after_its_actual_control_succeeds(self):
        (self.root / 'scripts').mkdir()
        (self.root / 'scripts/crispembed-runtime-lock.json').write_text('{}')
        argv = ['verify', '--platform', 'linux-x86_64']
        with patch.object(Path, 'cwd', return_value=self.root), \
                patch.object(verifier.subprocess, 'check_output', return_value=SOURCE), \
                patch.object(sys, 'argv', argv), \
                patch.object(verifier, 'inspect_package', side_effect=ValueError('package OCR failed')):
            with self.assertRaisesRegex(ValueError, 'package OCR failed'):
                verifier.main()
        path = self.root / 'CrispSorter_0.15.1_linux-x86_64.receipt.json'
        self.assertFalse(path.exists())
        with patch.object(Path, 'cwd', return_value=self.root), \
                patch.object(verifier.subprocess, 'check_output', return_value=SOURCE), \
                patch.object(sys, 'argv', argv), \
                patch.object(verifier, 'inspect_package', return_value={'checks': [{'result': {'version': '0.15.1'}}]}):
            verifier.main()
        self.assertEqual(json.loads(path.read_text())['ocr_qualification'], qualification.package_control())

    def test_workflow_keeps_original_license_gate_and_publication_requirement(self):
        workflow = (ROOT / '.github/workflows/release.yml').read_text()
        package = workflow.index('- name: Verify final packages and attach source/checksum receipts')
        licensed = workflow.index('- name: Qualify licensed OCR presets before publication (Linux)')
        self.assertLess(package, licensed)
        section = workflow[licensed:workflow.index('- name: Remove temporary desktop signing credentials', licensed)]
        self.assertIn("runner.os == 'Linux' && (github.event_name != 'workflow_dispatch' || inputs.publish)", section)
        self.assertIn('ocr_task_presets_live -- --ignored --nocapture --test-threads=1', section)
        self.assertIn('rm -f "$CRISPSORTER_OCR_RECEIPT"', section)
        self.assertLess(section.index('export CRISPSORTER_OCR_PACKAGE_RECEIPT='), section.index('cargo test'))
        self.assertIn('set -euo pipefail', section)
        self.assertNotIn('continue-on-error', section)
        self.assertNotIn('CRISPSORTER_ACCEPT_MODEL_LICENSE', section)
        self.assertNotIn('CRISPEMBED_ACCEPT_LICENSE', section)
        self.assertNotIn('--accept-license', section)
        self.assertIn("needs.release-required.result == 'success'", workflow.split('  publish:', 1)[1])
        producer = (ROOT / 'src-tauri/src/extractors/ocr_crispembed.rs').read_text()
        self.assertIn('"schema":"crispsorter-native-ocr-presets-v1"', producer)
        self.assertIn('"app_source":app_source', producer)
        self.assertIn('"sha256":fixture_sha256', producer)
        self.assertIn('"package_receipt":package_receipt', producer)

    def test_clean_package_control_provisions_only_explicit_permissive_models(self):
        def plan(installed):
            return {'engine': 'dbnet_trocr', 'missing': [] if installed else ['dbnet-det', 'trocr-printed'],
                    'models': [{'name': name, 'license': license, 'requires_consent': False, 'installed': installed}
                               for name, license in [('dbnet-det', 'apache-2.0'), ('trocr-printed', 'mit')]]}
        missing, ready = plan(False), plan(True)
        responses = [subprocess.CompletedProcess([], 0, stdout=json.dumps(value)) for value in [missing, ready]]
        env = {'CONTROL': 'preserved'}
        with patch.object(verifier.subprocess, 'run', side_effect=responses) as run:
            result = verifier.prepare_permissive_ocr_models(['packaged-app'], self.root, env)
        self.assertEqual(result['result'], 'permissive OCR model pair available')
        self.assertEqual(run.call_args_list[1].args[0], ['packaged-app', '--format', 'json', 'models', 'pull', 'dbnet_trocr'])
        self.assertTrue(all(call.kwargs['env'] == env for call in run.call_args_list))
        with patch.object(verifier.subprocess, 'run', return_value=responses[1]) as run:
            verifier.prepare_permissive_ocr_models(['packaged-app'], self.root, env)
            self.assertEqual(run.call_count, 1)
        for case in ['licensed', 'unknown', 'unexpected-model', 'incomplete-pull']:
            initial = copy.deepcopy(missing)
            if case == 'licensed': initial['models'][0]['requires_consent'] = True
            elif case == 'unknown': initial['models'][0]['license'] = 'unknown'
            elif case == 'unexpected-model': initial['models'][0]['name'] = 'qwen2vl-3b'
            responses = [subprocess.CompletedProcess([], 0, stdout=json.dumps(value)) for value in [initial, missing]]
            with self.subTest(case=case), patch.object(verifier.subprocess, 'run', side_effect=responses) as run:
                with self.assertRaises(ValueError):
                    verifier.prepare_permissive_ocr_models(['packaged-app'], self.root, env)
                self.assertEqual(run.call_count, 2 if case == 'incomplete-pull' else 1)


if __name__ == '__main__':
    unittest.main()
