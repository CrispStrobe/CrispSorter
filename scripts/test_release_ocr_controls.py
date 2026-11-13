"""Actual release-package and OCR licensing controls; no models or builds."""
import copy
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
import verify_release_artifacts as verifier

ROOT = Path(__file__).resolve().parents[1]
SOURCE = '1' * 40

class ReleaseOCRControls(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / 'src-tauri').mkdir()
        (self.root / 'src-tauri/tauri.conf.json').write_text('{"version":"0.15.1"}')
        self.package = self.root / 'target/release/bundle/deb/CrispSorter_0.15.1_amd64.deb'
        self.package.parent.mkdir(parents=True)
        self.package.write_bytes(b'nonexecutable package fixture')

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
            self.assertEqual(json.loads(path.read_text())['artifacts'][0]['checks'][0]['result']['version'], '0.15.1')

    def test_real_native_test_remains_required_without_report_or_consent_bypass(self):
        workflow = (ROOT / '.github/workflows/release.yml').read_text()
        section = workflow.split('- name: Qualify licensed OCR presets before publication (Linux)', 1)[1].split('- name: Remove temporary desktop signing credentials', 1)[0]
        self.assertIn("runner.os == 'Linux' && (github.event_name != 'workflow_dispatch' || inputs.publish)", section)
        self.assertIn('ocr_task_presets_live -- --ignored --nocapture --test-threads=1', section)
        for unwanted in ['continue-on-error', 'CRISPSORTER_ACCEPT_MODEL_LICENSE', 'CRISPEMBED_ACCEPT_LICENSE', '--accept-license', 'release_ocr_qualification.py', 'CRISPSORTER_OCR_PACKAGE_RECEIPT']:
            self.assertNotIn(unwanted, section)
        producer = (ROOT / 'src-tauri/src/extractors/ocr_crispembed.rs').read_text().split('    fn ocr_task_presets_live()', 1)[1].split('    fn ocr_pipeline_live_tesseract_stage()', 1)[0]
        self.assertIn('for preset in ["quick", "multilingual", "tables", "difficult"]', producer)
        self.assertIn('crate::model_provisioning::resolve_model', producer)
        self.assertIn('assert!(cases.iter().all(|case| case["matched"] == true)', producer)
        self.assertNotIn('CRISPSORTER_OCR_PACKAGE_RECEIPT', producer)
        self.assertNotIn('std::process::Command', producer)
        self.assertIn('Native OCR test diagnostics could not be written', producer)

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
