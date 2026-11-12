"""Small replay staging controls; never execute Android code or fetch models."""
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import sys
import tempfile
import unittest
from unittest.mock import patch
import zipfile

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'scripts'))
SPEC = importlib.util.spec_from_file_location('candidate_replay', ROOT / 'tools/android-candidate-boundary/stage.py')
replay = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(replay)


class CandidateReplayTests(unittest.TestCase):
    def fixture(self, root):
        harness, app = root / 'harness', root / 'app'
        pin = {'application_source': 'a' * 40, 'application_tree': 'b' * 40,
               'artifact_id': 7, 'artifact_name': 'candidate', 'run_id': 8,
               'apk_member': 'app.apk', 'apk_bytes': 3, 'apk_sha256': hashlib.sha256(b'apk').hexdigest(),
               'build_receipt_member': 'build.json', 'package': 'com.crispstrobe.crispsorter', 'abi': 'x86_64',
               'runtime_lock': 'scripts/candidate.json', 'native_source': 'c' * 40, 'ggml_source': 'd' * 40}
        png = b'bounded PNG fixture; no image decoding in staging'
        models = [{'name': 'dbnet-det'}, {'name': 'tesseract-eng'}]
        build = {'sorter_commit': pin['application_source'], 'sorter_tree': pin['application_tree'],
                 'platform': 'android', 'package': pin['package'], 'application_source_clean': True,
                 'artifact': {'kind': 'apk', 'abis': ['x86_64'], 'sha256': pin['apk_sha256'], 'bytes': 3},
                 'runtime_lock': {'path': pin['runtime_lock'], 'sha256': 'e' * 64},
                 'ggml_commit': pin['ggml_source'], 'candidate_reuse': {'fixture': True}}
        build_bytes = json.dumps(build).encode()
        pin['build_receipt_sha256'] = hashlib.sha256(build_bytes).hexdigest()
        files = {'app.apk': b'apk', 'build.json': build_bytes,
                 'runtime.json': b'{}', 'inputs.json': json.dumps({'models': models,
                    'fixture': {'png_sha256': hashlib.sha256(png).hexdigest()}}).encode(), 'page.png': png}
        archive = root / 'artifact.zip'
        with zipfile.ZipFile(archive, 'w') as zipped:
            for name, data in files.items(): zipped.writestr(name, data)
        pin.update(artifact_bytes=archive.stat().st_size, artifact_sha256=replay.digest(archive))
        pin['staged_members'] = {name: {'destination': destination, 'bytes': len(files[name]),
                                      'sha256': hashlib.sha256(files[name]).hexdigest()}
                                for name, destination in [('runtime.json', 'runtime-build.json'),
                                    ('inputs.json', 'inputs/inputs.json'), ('page.png', 'inputs/page.png')]}
        for checkout in (app, harness):
            path = checkout / 'tools/mobile-ocr-probe/models.json'; path.parent.mkdir(parents=True)
            path.write_text(json.dumps({'models': models}))
        path = harness / 'tools/android-startup-probe/stage.py'; path.parent.mkdir(parents=True)
        path.write_bytes((ROOT / 'tools/android-startup-probe/stage.py').read_bytes())
        path = harness / replay.PIN; path.parent.mkdir(parents=True)
        path.write_text(json.dumps(pin))
        metadata = {'id': 7, 'name': 'candidate', 'size_in_bytes': pin['artifact_bytes'],
                    'digest': 'sha256:' + pin['artifact_sha256'],
                    'workflow_run': {'id': 8, 'head_sha': pin['application_source']}}
        def git(source, *args):
            if args[0] == 'status': return ''
            if source == app: return pin['application_tree'] if 'HEAD^{tree}' in args else pin['application_source']
            return 'f' * 40
        return harness, app, archive, pin, metadata, git

    def run_stage(self, root, change=None):
        harness, app, archive, pin, metadata, git = self.fixture(root)
        if change: change(harness, app, archive, pin, metadata)
        with patch.object(replay.subprocess, 'check_output', return_value=(harness / replay.PIN).read_bytes()), \
             patch.object(replay, 'git', side_effect=git), \
             patch.object(replay, 'verified_embed_commit', return_value=pin['native_source']) as verify:
            value = replay.stage(archive, metadata, root / 'staged', app, harness)
            verify.assert_called_once_with(app, pin['runtime_lock'], root / 'staged/build-receipt.json',
                                           root / 'staged/runtime-build.json', pin['native_source'])
            return value

    def test_staging_binds_distinct_sources_exact_apk_fixture_and_receipts(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); value = self.run_stage(root)
            self.assertEqual(value['application_source'], 'a' * 40)
            self.assertEqual(value['harness_source'], 'f' * 40)
            self.assertFalse(value['application_rebuilt'])
            self.assertFalse(value['native_runtime_rebuilt'])
            self.assertEqual((root / 'staged/CrispSorter-ocr.apk').read_bytes(), b'apk')
            self.assertTrue((root / 'staged/inputs/page.png').is_file())

    def test_wrong_artifact_source_and_changed_app_baseline_are_rejected(self):
        for kind in ('artifact-source', 'app-baseline', 'member-hash'):
            def change(harness, app, archive, pin, metadata):
                if kind == 'artifact-source': metadata['workflow_run']['head_sha'] = '0' * 40
                elif kind == 'app-baseline': (app / 'tools/mobile-ocr-probe/models.json').write_text('{"models":[]}')
                else:
                    pin['staged_members']['page.png']['sha256'] = '0' * 64
                    (harness / replay.PIN).write_text(json.dumps(pin))
            with self.subTest(kind=kind), tempfile.TemporaryDirectory() as directory:
                with self.assertRaises(ValueError): self.run_stage(Path(directory), change)

    def test_route_has_no_build_or_fixture_model_download_step(self):
        workflow = (ROOT / '.github/workflows/ocr-mobile-native.yml').read_text()
        self.assertFalse((ROOT / '.github/workflows/ocr-android-candidate-boundary-replay.yml').exists())
        replay_job = workflow.split('  candidate-boundary-replay:\n', 1)[1]
        for forbidden in ('build_mobile_crispembed.py', 'tauri android build', 'cargo build', 'prepare.py'):
            self.assertNotIn(forbidden, replay_job)
        self.assertIn('ref: a26ea39776b5510f55a73f0a0fde3f54b0724b20', replay_job)
        self.assertNotIn('needs:', replay_job)
        self.assertIn("if: github.event_name == 'workflow_dispatch' && inputs.candidate_replay", replay_job)
        self.assertIn('"$REQUESTED_TARGET" != android-x86_64', replay_job)
        self.assertIn("  contracts:\n    if: github.event_name != 'workflow_dispatch' || !inputs.candidate_replay", workflow)
        self.assertIn("if: (github.ref_name == 'feat/ocr-document-studio' || github.event_name == 'workflow_dispatch') && (github.event_name != 'workflow_dispatch' || !inputs.candidate_replay)", workflow)
        self.assertIn('candidate_replay:\n        description:', workflow)
        self.assertRegex(workflow, r'candidate_replay:\n[^\n]*\n        type: boolean\n        default: false')
        concurrency = re.findall(r'(?m)^concurrency:\n(?:^  .*\n)+', workflow)
        self.assertEqual(concurrency, ["concurrency:\n  group: ${{ github.workflow }}-${{ github.ref }}\n"
                                     "  cancel-in-progress: ${{ github.ref_type != 'tag' && (github.event_name == 'push' || github.event_name == 'pull_request') }}\n"])
        self.assertEqual(len(re.findall(r'(?m)^\s*concurrency:', workflow)), 1)
        source = (ROOT / 'scripts/mobile_ocr_webview_smoke.mjs').read_text()
        self.assertIn("boundaryOnly ? [] : prepared.inputs.quality_fixtures.models", source)
        self.assertIn("if (boundaryOnly) assert(prepared.candidateRuntime", source)
        self.assertIn("receipt.status = 'passed';\n      return;\n    }\n\n    const recognized", source)


if __name__ == '__main__': unittest.main()
