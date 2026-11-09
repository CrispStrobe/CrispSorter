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
