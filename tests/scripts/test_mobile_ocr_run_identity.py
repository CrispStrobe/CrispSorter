"""Exercise the actual Android consumer launcher without an emulator or Node."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'scripts'))
from ocr_mobile_build_receipt import digest, git, runtime_lock_identity

PRODUCTION = 'scripts/ocr-mobile-runtime-lock.json'
CANDIDATE = 'scripts/ocr-mobile-error-propagation-candidate-lock.json'


class AndroidRuntimeIdentityTests(unittest.TestCase):
    def fixture(self, root, selected):
        lock, identity = runtime_lock_identity(ROOT, selected)
        runtime = {'source_commit': lock['source_commit'], 'ggml_commit': lock['ggml_commit'],
                   'target': 'android-x86_64'}
        runtime_path = root / 'ocr-mobile-runtime/runtime-build.json'
        runtime_path.parent.mkdir()
        runtime_path.write_text(json.dumps(runtime))
        build = {'runtime_lock': identity, 'sorter_commit': git(ROOT, 'rev-parse', 'HEAD'),
                 'crispembed_commit': lock['source_commit'], 'ggml_commit': lock['ggml_commit'],
                 'runtime': runtime, 'runtime_receipt_sha256': digest(runtime_path), 'platform': 'android'}
        build_path = root / 'ocr-mobile-app/build-receipt.json'
        build_path.parent.mkdir()
        build_path.write_text(json.dumps(build))
        # Record exactly what the real shell launcher sends to its Node consumer.
        node = root / 'node'
        node.write_text('#!' + sys.executable + '\nimport json,os,pathlib,sys\n'
                        'pathlib.Path(os.environ["CONSUMER_ARGS"]).write_text(json.dumps(sys.argv[1:]))\n')
        node.chmod(0o755)
        env = dict(os.environ, PATH=str(root) + os.pathsep + os.environ.get('PATH', ''),
                   RUNNER_TEMP=str(root), CONSUMER_ARGS=str(root / 'consumer.json'))
        env.pop('OCR_MOBILE_RUNTIME_LOCK', None)
        env.pop('OCR_MOBILE_RUNTIME_SOURCE', None)
        return lock, build, build_path, runtime_path, env

    def run_launcher(self, env):
        return subprocess.run(['bash', str(ROOT / 'tools/mobile-ocr-probe/run_android_app_commands.sh')],
                              env=env, capture_output=True, text=True, timeout=30)

    def test_actual_consumer_receives_default_production_and_selected_candidate_commits(self):
        for selected in (PRODUCTION, CANDIDATE):
            with self.subTest(selected=selected), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                lock, build, build_path, runtime_path, env = self.fixture(root, selected)
                if selected == CANDIDATE:
                    env.update(OCR_MOBILE_RUNTIME_LOCK=selected,
                               OCR_MOBILE_RUNTIME_SOURCE=lock['source_commit'])
                result = self.run_launcher(env)
                self.assertEqual(result.returncode, 0, result.stderr)
                args = json.loads((root / 'consumer.json').read_text())
                self.assertEqual(args[0], str(ROOT / 'scripts/mobile_ocr_webview_smoke.mjs'))
                options = dict(zip(args[1::2], args[2::2]))
                self.assertEqual(options['--embed-commit'], lock['source_commit'])
                self.assertEqual(options['--source-commit'], build['sorter_commit'])
                self.assertEqual(options['--build-receipt'], str(build_path))
                self.assertEqual(options['--runtime-receipt'], str(runtime_path))
                self.assertEqual(options['--serial'], 'emulator-5554')

    def test_mismatched_lock_source_and_receipts_fail_before_consumer_invocation(self):
        for mismatch in ('default-lock', 'workflow-source', 'lock-path', 'lock-hash',
                         'app-source', 'embed-source', 'ggml-source', 'runtime-receipt', 'target'):
            with self.subTest(mismatch=mismatch), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                lock, build, build_path, runtime_path, env = self.fixture(root, CANDIDATE)
                env.update(OCR_MOBILE_RUNTIME_LOCK=CANDIDATE, OCR_MOBILE_RUNTIME_SOURCE=lock['source_commit'])
                if mismatch == 'default-lock':
                    env.pop('OCR_MOBILE_RUNTIME_LOCK')
                    env.pop('OCR_MOBILE_RUNTIME_SOURCE')
                elif mismatch == 'workflow-source':
                    env['OCR_MOBILE_RUNTIME_SOURCE'] = '0' * 40
                elif mismatch == 'lock-path':
                    build['runtime_lock']['path'] = PRODUCTION
                elif mismatch == 'lock-hash':
                    build['runtime_lock']['sha256'] = '0' * 64
                elif mismatch == 'app-source':
                    build['sorter_commit'] = '0' * 40
                elif mismatch == 'embed-source':
                    build['crispembed_commit'] = '0' * 40
                elif mismatch == 'ggml-source':
                    build['ggml_commit'] = '0' * 40
                elif mismatch == 'runtime-receipt':
                    runtime_path.write_text('{}')
                elif mismatch == 'target':
                    build['runtime']['target'] = 'android-arm64'
                    runtime_path.write_text(json.dumps(build['runtime']))
                    build['runtime_receipt_sha256'] = digest(runtime_path)
                build_path.write_text(json.dumps(build))
                result = self.run_launcher(env)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse((root / 'consumer.json').exists())

    def test_workflow_passes_selected_lock_and_source_through_actual_runner(self):
        workflow = (ROOT / '.github/workflows/ocr-mobile-native.yml').read_text()
        step = workflow.split('- name: Execute native OCR, real app model installation and review export on Android', 1)[1]
        step = step.split('      - name:', 1)[0]
        self.assertIn('OCR_MOBILE_RUNTIME_LOCK: ${{ steps.runtime.outputs.lock }}', step)
        self.assertIn('OCR_MOBILE_RUNTIME_SOURCE: ${{ steps.runtime.outputs.source }}', step)
        self.assertIn('script: bash repos/CrispSorter/tools/mobile-ocr-probe/run_android.sh', step)
        runner = (ROOT / 'tools/mobile-ocr-probe/run_android.sh').read_text()
        self.assertIn('bash "$repo/tools/mobile-ocr-probe/run_android_app_commands.sh"', runner)
        self.assertNotIn('scripts/ocr-mobile-runtime-lock.json', runner)


if __name__ == '__main__':
    unittest.main()
