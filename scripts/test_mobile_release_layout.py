#!/usr/bin/env python3
"""Exercise release preparation with real clean sibling Git repositories."""
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

import check_mobile_release_layout as layout
from verify_release_artifacts import dependency_checkouts


def git(root, *args):
    return subprocess.check_output(['git', '-c', 'user.name=Layout Fixture',
        '-c', 'user.email=fixture@example.invalid', '-C', str(root), *args], text=True,
        stderr=subprocess.PIPE).strip()


def repository(path):
    path.mkdir(parents=True)
    git(path, 'init', '-q')
    (path / 'README').write_text('fixture\n')
    git(path, 'add', '.')
    git(path, 'commit', '-qm', 'fixture')
    return git(path, 'rev-parse', 'HEAD')


class Layout(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.fixture = tempfile.TemporaryDirectory()
        base = Path(cls.fixture.name)
        cls.embed = base / 'CrispEmbed'
        repository(cls.embed)
        ggml = repository(cls.embed / 'ggml')
        git(cls.embed, 'update-index', '--add', '--cacheinfo', '160000,' + ggml + ',ggml')
        for name in ['crispembed', 'crispembed-sys']:
            (cls.embed / name).mkdir()
            (cls.embed / name / 'Cargo.toml').write_text('[package]\nname="' + name + '"\nversion="0.1.0"\n')
        git(cls.embed, 'add', '.')
        git(cls.embed, 'commit', '-qm', 'native source')
        cls.embed_sha = git(cls.embed, 'rev-parse', 'HEAD')
        for sibling, packages in [('CrispASR', ['crispasr', 'crispasr-sys']),
                                   ('crisp-docx', ['crates/crisp-docx-core', 'crates/crisp-docx-llm', 'crates/crisp-docx-align'])]:
            root = base / sibling
            repository(root)
            for package in packages:
                (root / package).mkdir(parents=True)
                (root / package / 'Cargo.toml').write_text('[package]\nname="' + Path(package).name + '"\nversion="0.1.0"\n')
            git(root, 'add', '.')
            git(root, 'commit', '-qm', 'packages')
            git(root, 'tag', 'fixture-v1')
        app = base / 'CrispSorter'
        repository(app)
        (app / 'src-tauri').mkdir()
        (app / 'scripts').mkdir()
        (app / 'Cargo.lock').write_text('version = 4\n')
        lines = ['[dependencies]']
        for sibling, names in [('CrispEmbed', ['crispembed', 'crispembed-sys']),
                                ('CrispASR', ['crispasr', 'crispasr-sys']),
                                ('crisp-docx', ['crates/crisp-docx-core', 'crates/crisp-docx-llm', 'crates/crisp-docx-align'])]:
            for name in names:
                lines.append(Path(name).name + ' = { path = "../../' + sibling + '/' + name + '" }')
        (app / 'src-tauri/Cargo.toml').write_text('[package]\nname="fixture-app"\nversion="0.1.0"\nedition="2021"\n' + '\n'.join(lines) + '\n')
        for manifest in base.rglob('Cargo.toml'):
            (manifest.parent / 'src').mkdir(exist_ok=True)
            (manifest.parent / 'src/lib.rs').write_text('pub fn fixture() {}\n')
        for repo in ['CrispEmbed', 'CrispASR', 'crisp-docx']:
            git(base / repo, 'add', '.')
            git(base / repo, 'commit', '-qm', 'crate targets')
            if repo != 'CrispEmbed':
                git(base / repo, 'tag', '-f', 'fixture-v1')
        cls.embed_sha = git(cls.embed, 'rev-parse', 'HEAD')
        subprocess.run(['cargo', 'generate-lockfile', '--offline', '--manifest-path', str(app / 'src-tauri/Cargo.toml')], check=True, capture_output=True)
        lock = {'source_commit': cls.embed_sha, 'ggml_commit': ggml}
        for filename in ['crispembed-runtime-lock.json', 'ocr-mobile-runtime-lock.json']:
            (app / 'scripts' / filename).write_text(json.dumps(lock))
        git(app, 'add', '.')
        git(app, 'commit', '-qm', 'application')

    @classmethod
    def tearDownClass(cls):
        cls.fixture.cleanup()

    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.base = Path(temporary.name) / 'repos'
        shutil.copytree(self.fixture.name, self.base)
        self.app = self.base / 'CrispSorter'

    def check(self):
        return layout.check(self.app, self.embed_sha, 'fixture-v1', 'fixture-v1')

    def test_real_sibling_layout_preserves_source_and_receipt_identity(self):
        before = (self.app / 'src-tauri/Cargo.toml').read_bytes()
        result = self.check()
        self.assertTrue(result['application_source_clean'])
        self.assertEqual(result['app_source'], git(self.app, 'rev-parse', 'HEAD'))
        self.assertEqual(result['dependency_sources']['CrispEmbed'], self.embed_sha)
        self.assertEqual(len(result['checked_dependencies']), 7)
        self.assertEqual((self.app / 'src-tauri/Cargo.toml').read_bytes(), before)
        metadata = json.loads(subprocess.check_output(['cargo', 'metadata', '--no-deps', '--offline', '--locked', '--format-version', '1', '--manifest-path', str(self.app / 'src-tauri/Cargo.toml')], text=True, stderr=subprocess.PIPE))
        self.assertEqual(len(metadata['packages'][0]['dependencies']), 7)
        self.assertEqual(metadata['packages'][0]['name'], 'fixture-app')
        self.assertEqual(git(self.app, 'status', '--porcelain'), '')
        self.assertEqual(set(dependency_checkouts(self.app)), {'CrispEmbed', 'CrispASR', 'crisp-docx'})

    def test_rewritten_dirty_manifest_rejected(self):
        path = self.app / 'src-tauri/Cargo.toml'
        path.write_text(path.read_text().replace('../../CrispEmbed', '../_sibling/CrispEmbed'))
        with self.assertRaisesRegex(ValueError, 'source or dependency lock changed'):
            self.check()

    def test_committed_rewritten_manifest_cannot_bypass_path_validation(self):
        path = self.app / 'src-tauri/Cargo.toml'
        path.write_text(path.read_text().replace('../../CrispEmbed', '../_sibling/CrispEmbed'))
        git(self.app, 'add', '.')
        git(self.app, 'commit', '-qm', 'bad layout')
        with self.assertRaisesRegex(ValueError, 'missing or rewritten'):
            self.check()

    def test_missing_package_rejected(self):
        (self.base / 'CrispASR/crispasr/Cargo.toml').unlink()
        # Restore clean sibling state while leaving its committed layout unsupported.
        git(self.base / 'CrispASR', 'add', '.')
        git(self.base / 'CrispASR', 'commit', '-qm', 'remove package')
        git(self.base / 'CrispASR', 'tag', '-f', 'fixture-v1')
        with self.assertRaisesRegex(ValueError, 'unavailable'):
            self.check()

    def test_dirty_dependency_rejected(self):
        (self.base / 'crisp-docx/README').write_text('changed\n')
        with self.assertRaisesRegex(ValueError, 'clean pinned checkout'):
            self.check()

    def test_mismatched_dependency_ref_rejected(self):
        git(self.base / 'CrispASR', 'commit', '--allow-empty', '-qm', 'unselected revision')
        with self.assertRaisesRegex(ValueError, 'clean pinned checkout'):
            self.check()

    def test_runtime_candidate_cannot_replace_production_lock(self):
        with self.assertRaisesRegex(ValueError, 'pins must agree'):
            layout.check(self.app, 'candidate', 'fixture-v1', 'fixture-v1')

    def test_independent_mobile_candidate_does_not_block_standard_recipe(self):
        path = self.app / 'scripts/ocr-mobile-runtime-lock.json'
        value = json.loads(path.read_text())
        value['source_commit'] = 'candidate'
        path.write_text(json.dumps(value))
        git(self.app, 'add', '.')
        git(self.app, 'commit', '-qm', 'independent candidate')
        self.assertFalse(self.check()['mobile_qualification_lock_matches_release'])
        with self.assertRaisesRegex(ValueError, 'pins must agree for OCR'):
            layout.check(self.app, self.embed_sha, 'fixture-v1', 'fixture-v1', require_ocr_lock=True)

    def test_desktop_nested_dependency_discovery_preserved(self):
        nested = self.app / '_sibling/CrispEmbed'
        nested.mkdir(parents=True)
        self.assertEqual(dependency_checkouts(self.app)['CrispEmbed'], nested)

    def test_workflow_mobile_paths_resolve_without_source_rewriting(self):
        workflow = (Path(__file__).resolve().parents[1] / '.github/workflows/release.yml').read_text()
        for name, end in [('release-android', 'release-ios'), ('release-ios', 'release-macos-appstore')]:
            job = workflow.split('\n  ' + name + ':', 1)[1].split('\n  ' + end + ':', 1)[0]
            self.assertIn('working-directory: repos/CrispSorter', job)
            self.assertIn("workspaces: 'repos/CrispSorter -> target'", job)
            for checkout in ['CrispSorter', 'CrispEmbed', 'CrispASR', 'crisp-docx']:
                self.assertIn('path: repos/' + checkout, job)
            self.assertNotIn('_sibling/', job)
            self.assertNotIn('Rewrite sibling path deps', job)
            self.assertNotIn('github.workspace', job)
            self.assertNotIn('$GITHUB_WORKSPACE', job)
            self.assertIn('submodules: recursive', job)
            self.assertIn('check_mobile_release_layout.py', job)


if __name__ == '__main__':
    unittest.main()
