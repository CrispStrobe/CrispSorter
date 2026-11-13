#!/usr/bin/env python3
"""Exercise locked ort-sys DLL staging destinations without native builds."""
import hashlib
import json
from pathlib import Path, PureWindowsPath
import re
import shutil
import tempfile
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[1]
FIXTURES = ROOT / 'tests/fixtures/windows-ort-layout'


class WindowsOrtLayoutTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.evidence = json.loads((FIXTURES / 'evidence.json').read_text())
        cls.upstream = (FIXTURES / 'ort-sys-rc11-dynamic-link.rs').read_text()
        cls.workflow = (ROOT / '.github/workflows/release.yml').read_text()
        cls.ancestor = int(re.search(r'out_dir\.ancestors\(\)\.nth\((\d+)\)', cls.upstream)[1])
        cls.children = re.findall(r'out_dir\.join\("([^"]+)"\)', cls.upstream)
        array = re.search(r'\$destinations = @\(([^\n]+)\)', cls.workflow)[1]
        cls.destinations = re.findall(r'"([^"]+)"', array)

    def test_fixture_is_bound_to_the_exact_locked_dependency_and_observed_failure(self):
        self.assertFalse(any(key.endswith('_id') for key in self.evidence), 'Hosted execution identifiers must not be retained')
        document = (ROOT / 'docs/windows-ort-release-compatibility.md').read_text()
        self.assertIsNone(re.search(r'\b[0-9]{9,}\b', document), 'Documentation must not retain numeric hosted execution identifiers')
        package = next(p for p in tomllib.loads((ROOT / 'Cargo.lock').read_text())['package'] if p['name'] == 'ort-sys')
        dependency = self.evidence['dependency']
        self.assertEqual(package['version'], dependency['version'])
        self.assertEqual(package['checksum'], dependency['crate_checksum'])
        self.assertEqual(hashlib.sha256((FIXTURES / 'ort-sys-rc11-dynamic-link.rs').read_bytes()).hexdigest(), dependency['source_sha256'])
        observed = PureWindowsPath(self.evidence['observed_out_dir'])
        actual = observed.parents[self.ancestor - 1] / 'DirectML.dll'
        self.assertEqual(actual, PureWindowsPath(self.evidence['observed_copied_dll']))
        self.assertNotEqual(actual.parent, PureWindowsPath('target/release'))

    def test_windows_pin_selects_released_legacy_layout_without_changing_other_platforms(self):
        step = self.workflow.split('- name: Install Rust stable\n', 1)[1].split('      - name:', 1)[0]
        version = self.evidence['compatibility_toolchain']
        self.assertIn("toolchain: ${{ runner.os == 'Windows' && '" + version + "' || 'stable' }}", step)
        manifest = self.evidence['release_manifest']
        self.assertTrue(all(manifest[key] for key in ('rust_available', 'rustc_available', 'cargo_available')))
        self.assertEqual(version, '1.99.1')
        legacy = PureWindowsPath('target/release/build/ort-sys-79359be6031be721/out')
        self.assertEqual(legacy.parents[self.ancestor - 1], PureWindowsPath('target/release'))
        self.assertEqual(self.evidence['locked_registry_msrv_audit']['highest_declared'], '1.98')

    def test_actual_workflow_parents_allow_locked_dll_copy_for_app_examples_and_tests(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            dll = root / 'DirectML.dll'
            dll.write_bytes(b'nonexecutable DLL staging fixture')
            legacy = root / 'target/release/build/ort-sys-fixture/out'
            profile = legacy.parents[self.ancestor - 1]
            profile.mkdir(parents=True)
            (profile / 'deps').mkdir()
            # Existing workflow omitted examples; reproduce the copy fallback's
            # actual filesystem error before applying its corrected directory list.
            with self.assertRaises(FileNotFoundError):
                shutil.copyfile(dll, profile / 'examples/DirectML.dll')
            for destination in self.destinations:
                (root / destination).mkdir(parents=True, exist_ok=True)
            for destination in [profile, *(profile / child for child in self.children)]:
                shutil.copyfile(dll, destination / dll.name)
                self.assertEqual((destination / dll.name).read_bytes(), dll.read_bytes())


if __name__ == '__main__':
    unittest.main()
