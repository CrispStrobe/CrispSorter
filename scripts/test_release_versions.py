#!/usr/bin/env python3
"""Version-consistency regressions without dependencies, builds or GitHub writes."""
import json
from pathlib import Path
import tempfile
import unittest

from check_release_versions import check_versions


class ReleaseVersions(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        (self.root / 'src-tauri').mkdir()
        self.write_json('src-tauri/tauri.conf.json', {'version': '0.15.1'})
        self.write_json('package.json', {'name': 'tauri-app', 'version': '0.15.1'})
        self.write_json('package-lock.json', {'name': 'tauri-app', 'version': '0.15.1',
                                            'packages': {'': {'name': 'tauri-app', 'version': '0.15.1'}}})
        (self.root / 'src-tauri/Cargo.toml').write_text('[package]\nname="crispsorter"\nversion="0.15.1"\n')
        (self.root / 'Cargo.toml').write_text('[workspace.package]\nversion="0.1.39"\n')
        self.lock = '[[package]]\nname="crispsorter"\nversion="0.15.1"\n\n[[package]]\nname="unrelated"\nversion="0.15.0"\nsource="registry+example"\n'
        (self.root / 'Cargo.lock').write_text(self.lock)

    def write_json(self, name, value):
        (self.root / name).write_text(json.dumps(value))

    def test_consistent_versions_ignore_independent_workspace_and_dependency_versions(self):
        before = {path: path.read_bytes() for path in self.root.rglob('*') if path.is_file()}
        self.assertEqual(check_versions(self.root), '0.15.1')
        self.assertEqual(before, {path: path.read_bytes() for path in before})

    def test_each_frontend_declaration_must_match(self):
        for name, change in [('package.json', lambda data: data.update(version='0.15.0')),
                             ('package-lock.json', lambda data: data.update(version='0.15.0')),
                             ('package-lock.json', lambda data: data['packages'][''].update(version='0.15.0'))]:
            with self.subTest(name=name):
                path = self.root / name
                original = path.read_text()
                value = json.loads(original)
                change(value)
                self.write_json(name, value)
                with self.assertRaisesRegex(ValueError, 'version does not match'):
                    check_versions(self.root)
                path.write_text(original)

    def test_cargo_manifest_and_local_lock_entry_must_match(self):
        path = self.root / 'src-tauri/Cargo.toml'
        path.write_text(path.read_text().replace('0.15.1', '0.15.0'))
        with self.assertRaisesRegex(ValueError, 'Cargo.toml version'):
            check_versions(self.root)
        path.write_text(path.read_text().replace('0.15.0', '0.15.1'))
        for lock in [self.lock.replace('version="0.15.1"', 'version="0.15.0"'),
                     self.lock + '\n[[package]]\nname="crispsorter"\nversion="0.15.1"\n',
                     self.lock.replace('name="crispsorter"', 'name="other-app"')]:
            with self.subTest(lock=lock):
                (self.root / 'Cargo.lock').write_text(lock)
                with self.assertRaisesRegex(ValueError, 'exactly one matching local'):
                    check_versions(self.root)

    def test_npm_lock_identity_must_match(self):
        for field in ['root', 'top']:
            path = self.root / 'package-lock.json'
            original = path.read_text()
            data = json.loads(original)
            (data['packages'][''] if field == 'root' else data)['name'] = 'another-app'
            self.write_json('package-lock.json', data)
            with self.assertRaisesRegex(ValueError, 'npm lock identity'):
                check_versions(self.root)
            path.write_text(original)

    def test_release_requires_an_exact_numeric_version(self):
        for version in [None, 'v0.15.1', '0.15', '0.15.01', '0.15.1-beta', {'path': 'version.txt'}]:
            with self.subTest(version=version):
                self.write_json('src-tauri/tauri.conf.json', {'version': version})
                with self.assertRaisesRegex(ValueError, 'exact numeric'):
                    check_versions(self.root)


if __name__ == '__main__':
    unittest.main()
