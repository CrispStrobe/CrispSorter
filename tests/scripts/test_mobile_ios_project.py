"""The final Xcode link must retain the qualified static runtime path."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).resolve().parents[2] / 'scripts/prepare_ocr_ios_project.py'
SPEC = importlib.util.spec_from_file_location('prepare_ocr_ios_project', SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class IosProjectTests(unittest.TestCase):
    def test_final_link_carries_static_framework_and_preserves_other_settings(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            project = root / 'project.yml'
            original = '# preserved\nsettings:\n' + MODULE.ANCHOR
            project.write_text(original)
            (root / 'application.xcodeproj').mkdir()
            framework = root / 'path with spaces' / 'onnxruntime'
            framework.parent.mkdir()
            framework.write_bytes(b'configuration fixture, never linked')
            with patch.object(MODULE.subprocess, 'run') as run:
                MODULE.prepare(project, framework)
                written = project.read_text()
                MODULE.prepare(project, framework)
            self.assertEqual(project.read_text(), written)
            self.assertTrue(written.startswith(original))
            flags = next(line.split(': ', 1)[1] for line in written.splitlines() if 'OTHER_LDFLAGS:' in line)
            self.assertIn('-lc++ -framework CoreML', json.loads(flags))
            self.assertIn(json.dumps(str(framework)), json.loads(flags))
            self.assertEqual(run.call_count, 2)
            self.assertTrue((root / 'application.xcodeproj/project.xcworkspace/contents.xcworkspacedata').is_file())

    def test_draft_retains_existing_native_names_without_changing_linking_or_signing(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            project = root / 'project.yml'
            original = '# preserved\nsettings:\n' + MODULE.ANCHOR
            project.write_text(original)
            (root / 'application.xcodeproj').mkdir()
            framework = root / 'onnxruntime'
            framework.write_bytes(b'configuration fixture, never linked')
            with patch.object(MODULE.subprocess, 'run'):
                MODULE.prepare(project, framework)
            standard = project.read_text()
            project.write_text(original)
            with patch.object(MODULE.subprocess, 'run'):
                MODULE.prepare(project, framework, retain_native_symbols=True)
                draft = project.read_text()
                MODULE.prepare(project, framework, retain_native_symbols=True)
            self.assertEqual(project.read_text(), draft)
            self.assertEqual(draft.replace('        # Signed OCR draft C API provenance\n        STRIP_STYLE: debugging\n', ''), standard)
            self.assertNotIn('force_load', draft)
            self.assertNotIn('exported_symbol', draft)
            self.assertNotIn('DEAD_CODE_STRIPPING', draft)
            self.assertNotIn('STRIP_STYLE', standard)

    def test_retention_rejects_missing_framework_or_existing_strip_override(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            project = root / 'project.yml'
            project.write_text(MODULE.ANCHOR)
            with self.assertRaisesRegex(ValueError, 'exact qualified'):
                MODULE.prepare(project, retain_native_symbols=True)
            framework = root / 'onnxruntime'
            framework.write_bytes(b'fixture')
            original = MODULE.ANCHOR + '        STRIP_STYLE: all\n'
            project.write_text(original)
            with self.assertRaisesRegex(ValueError, 'already defines strip style'):
                MODULE.prepare(project, framework, retain_native_symbols=True)
            self.assertEqual(project.read_text(), original)

    def test_only_signed_ocr_draft_requests_retention(self):
        root = Path(__file__).resolve().parents[2]
        workflow = (root / '.github/workflows/release.yml').read_text()
        self.assertEqual(workflow.count('--retain-native-symbols'), 1)
        init = workflow.split('      - name: Init iOS project', 1)[1].split('      - name:', 1)[0]
        draft, standard = init.split('          else', 1)
        self.assertIn('ocr-draft', draft)
        self.assertIn('--retain-native-symbols', draft)
        self.assertNotIn('--retain-native-symbols', standard)
        verifier = (root / 'scripts/ocr_mobile_build_receipt.py').read_text()
        self.assertIn("symbol_proof = defined_code_symbol(artifact / executable)", verifier)
        self.assertIn("'defined_app_symbol_proof': symbol_proof", verifier)

    def test_conflicting_generated_settings_are_not_silently_replaced(self):
        with tempfile.TemporaryDirectory() as directory:
            project = Path(directory) / 'project.yml'
            original = MODULE.MARKER + '        OTHER_LDFLAGS: "unexpected"\n'
            project.write_text(original)
            with self.assertRaisesRegex(ValueError, 'differ'):
                MODULE.prepare(project)
            self.assertEqual(project.read_text(), original)


if __name__ == '__main__':
    unittest.main()
