import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('qualification', ROOT / 'scripts/mobile_qualification_lock.py')
qualification = importlib.util.module_from_spec(spec)
spec.loader.exec_module(qualification)


class QualificationLockTests(unittest.TestCase):
    def test_release_and_candidate_sources_are_selected_without_mutation(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for filename, source in [('release.json', 'a' * 40), ('candidate.json', 'b' * 40)]:
                data = json.dumps({'source_commit': source, 'ggml_commit': 'c' * 40}).encode()
                (root / filename).write_bytes(data)
                with patch.object(qualification.subprocess, 'check_output', return_value=data):
                    self.assertEqual(qualification.select_lock(filename, root),
                                     {'source': source, 'lock': filename})
                self.assertEqual((root / filename).read_bytes(), data)

    def test_rejects_branches_short_ids_missing_submodule_and_modified_lock(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / 'lock.json'
            for source, ggml in [('main', 'c' * 40), ('a' * 8, 'c' * 40), ('a' * 40, None)]:
                data = json.dumps({'source_commit': source, 'ggml_commit': ggml}).encode()
                path.write_bytes(data)
                with patch.object(qualification.subprocess, 'check_output', return_value=data):
                    with self.assertRaisesRegex(ValueError, 'full immutable'):
                        qualification.select_lock('lock.json', root)
            with patch.object(qualification.subprocess, 'check_output', return_value=b'{}'):
                with self.assertRaisesRegex(ValueError, 'committed bytes'):
                    qualification.select_lock('lock.json', root)

    def test_rejects_absolute_parent_and_escaping_symlink_paths(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / 'repo'
            root.mkdir()
            outside = root.parent / 'outside.json'
            outside.write_text('{}')
            (root / 'link.json').symlink_to(outside)
            for path in [str(outside), '../outside.json', 'link.json', 'lock.json\nsource=injected']:
                with self.assertRaises(ValueError):
                    qualification.select_lock(path, root)

    def test_workflow_uses_one_selected_lock_for_checkout_and_build(self):
        workflow = (ROOT / '.github/workflows/mobile-crispembed-qualification.yml').read_text()
        self.assertIn('default: scripts/crispembed-runtime-lock.json', workflow)
        self.assertIn('ref: ${{ steps.runtime.outputs.source }}', workflow)
        self.assertIn('RUNTIME_LOCK: ${{ steps.runtime.outputs.lock }}', workflow)
        self.assertIn('--lock "$RUNTIME_LOCK"', workflow)
        self.assertNotIn('78493a31fc3043f3af4ab6fd2f197a32da6f07f0', workflow)


if __name__ == '__main__':
    unittest.main()
