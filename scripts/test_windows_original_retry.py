#!/usr/bin/env python3
"""Source/draft/receipt controls for the original-source retry; no builds/API calls."""
import copy
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import prepare_windows_original_retry as retry

ROOT = Path(__file__).resolve().parents[1]
SOURCE = 'b739a3c0b70f39d3ae19e1bfcb412f3b44f77794'


class WindowsRetryTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        root = Path(self.temp.name)
        self.app, self.tooling = root / 'app', root / 'tooling'
        self.pin = json.loads((ROOT / retry.PIN).read_text())
        self.pin_bytes = (ROOT / retry.PIN).read_bytes()
        self.data = {path: subprocess.check_output(['git', '-C', str(ROOT), 'show', SOURCE + ':' + path])
                     for path in [*self.pin['files'], '.github/workflows/release.yml']}
        for path, data in self.data.items():
            p = self.app / path; p.parent.mkdir(parents=True, exist_ok=True); p.write_bytes(data)
        p = self.tooling / retry.PIN; p.parent.mkdir(parents=True); p.write_bytes(self.pin_bytes)
        self.workflow = (ROOT / '.github/workflows/release.yml').read_bytes()
        self.env = {'GITHUB_EVENT_NAME': 'workflow_dispatch', 'CRISPSORTER_RELEASE_SELECTOR': retry.SELECTOR,
                    'CRISPSORTER_RELEASE_PUBLISH': 'false', 'CRISPSORTER_RELEASE_MAC_APPSTORE': 'false',
                    'CRISPSORTER_MOBILE_RELEASE_PROFILE': 'standard', 'GITHUB_WORKFLOW_SHA': 'a' * 40}

    def committed(self, root, path):
        if root == self.tooling and path == retry.PIN: return self.pin_bytes
        if root == self.tooling and path == '.github/workflows/release.yml': return self.workflow
        return self.data[path]

    def git(self, root, *args):
        if args[0] == 'status': return ''
        if root == self.app:
            return self.pin['source_tree'] if 'HEAD^{tree}' in args else SOURCE
        return 'b' * 40 if 'HEAD^{tree}' in args else 'a' * 40

    def verify(self, phase='prepare'):
        with patch.object(retry, 'committed', side_effect=self.committed), \
                patch.object(retry, 'git', side_effect=self.git), \
                patch.object(retry, 'lookup_draft') as lookup:
            result = retry.verify(self.app, self.tooling, 'Example/Project', self.env, phase)
            lookup.assert_called_once_with('Example/Project', self.pin)
            return result

    def test_exact_source_and_workflow_are_distinct_and_no_execution_identifiers_are_retained(self):
        proof = self.verify()
        self.assertEqual(proof['app_source'], SOURCE)
        self.assertEqual(proof['workflow_source'], 'a' * 40)
        self.assertEqual(proof['version'], '0.15.1')
        self.assertEqual(proof['cargo_lock']['original_sha256'], self.pin['files']['Cargo.lock'])
        self.assertEqual(proof['cargo_lock']['effective_sha256'], self.pin['files']['Cargo.lock'])
        for key in ('draft_id', 'run_id', 'github_run_id', 'failed_run_id'):
            self.assertNotIn(key, proof)
            self.assertNotIn(key, self.pin)

    def test_invalid_mode_or_publication_flags_fail_before_draft_lookup(self):
        for key, value in [('GITHUB_EVENT_NAME', 'push'), ('CRISPSORTER_RELEASE_SELECTOR', 'windows'),
                           ('CRISPSORTER_RELEASE_PUBLISH', 'true'), ('CRISPSORTER_RELEASE_MAC_APPSTORE', 'true'),
                           ('CRISPSORTER_MOBILE_RELEASE_PROFILE', 'ocr'), ('GITHUB_WORKFLOW_SHA', 'c' * 40)]:
            with self.subTest(key=key), patch.dict(self.env, {key: value}), patch.object(retry, 'lookup_draft') as lookup:
                with self.assertRaises(ValueError): self.verify()
                lookup.assert_not_called()

    def test_changed_version_lock_recipe_or_retry_pin_is_rejected(self):
        for case in ('app-version', 'app-lock', 'recipe', 'pin'):
            with self.subTest(case=case):
                original_data = copy.deepcopy(self.data)
                original_workflow = self.workflow
                if case == 'app-version': (self.app / 'package.json').write_text('{}')
                elif case == 'app-lock': (self.app / 'Cargo.lock').write_text('modified')
                elif case == 'recipe': self.workflow = self.workflow.replace(b'--features desktop,sidecars', b'--features changed,sidecars')
                else: (self.tooling / retry.PIN).write_text('{}')
                with self.assertRaises(ValueError): self.verify()
                self.data, self.workflow = original_data, original_workflow
                for path, data in self.data.items(): (self.app / path).write_bytes(data)
                (self.tooling / retry.PIN).write_bytes(self.pin_bytes)

    def test_build_phase_allows_only_equivalent_sibling_path_rewrite_and_keeps_lock_exact(self):
        manifest = self.app / 'src-tauri/Cargo.toml'
        original = manifest.read_bytes()
        rewritten = original
        for sibling in ('CrispEmbed', 'CrispASR', 'crisp-docx'):
            rewritten = rewritten.replace(('../../' + sibling + '/').encode(), ('../_sibling/' + sibling + '/').encode())
        manifest.write_bytes(rewritten)
        self.verify('check')
        manifest.write_bytes(rewritten.replace(b'default = []', b'default = ["desktop"]'))
        with self.assertRaisesRegex(ValueError, 'version/dependency'): self.verify('check')
        manifest.write_bytes(rewritten)
        (self.app / 'Cargo.lock').write_text('changed lock')
        with self.assertRaisesRegex(ValueError, 'Cargo.lock'): self.verify('seal')

    def canonical_lock(self):
        # Frozen independently reproduced Cargo reorder; no graph edit or
        # arbitrary TOML serializer is used to manufacture a passing lock.
        return self.data['Cargo.lock'].replace(
            b' "rustls",\n "rustls-native-certs",\n "rten",\n "rusqlite",\n',
            b' "rten",\n "rusqlite",\n "rustls",\n "rustls-native-certs",\n', 1)

    def test_exact_canonical_lock_is_post_build_only_and_binds_both_hashes_and_full_graph(self):
        original, canonical = self.data['Cargo.lock'], self.canonical_lock()
        self.assertEqual(hashlib.sha256(canonical).hexdigest(),
                         '2059e4df62369fb4e56db8ad6b539361d02d9f17f635ccc41da2607dbc813210')
        self.assertEqual(retry.lock_metadata_graph(original), retry.lock_metadata_graph(canonical))
        (self.app / 'Cargo.lock').write_bytes(canonical)
        with self.assertRaisesRegex(ValueError, 'Pre-build Cargo.lock'): self.verify('prepare')
        for phase in ('check', 'seal'):
            proof = self.verify(phase)
            self.assertEqual(proof['cargo_lock']['original_sha256'], self.pin['files']['Cargo.lock'])
            self.assertEqual(proof['cargo_lock']['effective_sha256'], self.pin['post_build_cargo_lock']['effective_sha256'])
            self.assertEqual(proof['cargo_lock']['lock_metadata_graph_sha256'], self.pin['post_build_cargo_lock']['lock_metadata_graph_sha256'])
            self.assertTrue(proof['cargo_lock']['lock_metadata_graph_equal'])
            self.assertTrue(proof['cargo_lock']['canonical_dependency_order_applied'])

    def test_other_reorders_graph_metadata_and_whitespace_changes_remain_rejected(self):
        original, canonical = self.data['Cargo.lock'], self.canonical_lock()
        changes = [canonical + b'\n', canonical.replace(b'version = 4', b'version = 3', 1),
                   canonical.replace(b' "rustls",', b' "unexpected-dependency",', 1),
                   canonical.replace(b'checksum = "', b'checksum = "0', 1),
                   canonical.replace(b'source = "registry+', b'source = "other+', 1),
                   canonical.replace(b' "rten",\n "rusqlite",', b' "rusqlite",\n "rten",', 1)]
        for changed in changes:
            with self.subTest(sha=hashlib.sha256(changed).hexdigest()):
                with self.assertRaises(ValueError): retry.verify_cargo_lock(original, changed, self.pin, 'check')
                # An altered expected hash alone cannot waive exact byte
                # transformation and complete metadata/graph equality.
                pin = copy.deepcopy(self.pin)
                pin['post_build_cargo_lock']['effective_sha256'] = hashlib.sha256(changed).hexdigest()
                with self.assertRaises(ValueError): retry.verify_cargo_lock(original, changed, pin, 'seal')

    def test_draft_must_exist_uniquely_be_unpublished_and_target_original_source(self):
        good = {'tag_name': 'v0.15.1', 'draft': True, 'prerelease': False, 'target_commitish': SOURCE}
        for releases, success in [([], False), ([good], True), ([good, good], False),
                                  ([{**good, 'draft': False}], False),
                                  ([{**good, 'prerelease': True}], False),
                                  ([{**good, 'target_commitish': 'main'}], False),
                                  ([{**good, 'target_commitish': 'c' * 40}], False)]:
            with self.subTest(releases=releases), \
                    patch.object(retry.subprocess, 'check_output', return_value=json.dumps([[], releases])), \
                    patch.object(retry, 'validate_release_target') as target:
                if success:
                    retry.lookup_draft('Example/Project', self.pin)
                    target.assert_called_once_with('Example/Project', 'v0.15.1', SOURCE)
                else:
                    with self.assertRaises(ValueError): retry.lookup_draft('Example/Project', self.pin)
                    target.assert_not_called()

    def test_tag_target_check_and_api_errors_are_never_waived(self):
        good = {'tag_name': 'v0.15.1', 'draft': True, 'prerelease': False, 'target_commitish': SOURCE}
        with patch.object(retry.subprocess, 'check_output', return_value=json.dumps([[good]])), \
                patch.object(retry, 'validate_release_target', side_effect=ValueError('tag mismatch')):
            with self.assertRaisesRegex(ValueError, 'tag mismatch'): retry.lookup_draft('Example/Project', self.pin)
        with patch.object(retry.subprocess, 'check_output', side_effect=RuntimeError('API unavailable')):
            with self.assertRaisesRegex(RuntimeError, 'API unavailable'): retry.lookup_draft('Example/Project', self.pin)

    def test_final_receipt_binds_original_source_and_actual_portable_bytes_to_workflow(self):
        (self.app / 'Cargo.lock').write_bytes(self.canonical_lock())
        proof = self.verify('seal')
        package = self.app / 'CrispSorter_0.15.1_x64-windows-portable-unsigned.zip'
        package.write_bytes(b'nonexecutable portable archive fixture')
        runtime_lock = json.loads(self.data['scripts/crispembed-runtime-lock.json'])
        value = {'schema': 'crispsorter-release-receipt-v1', 'platform': 'windows-x86_64',
                 'app_source': SOURCE, 'version': '0.15.1', 'runtime_lock': runtime_lock,
                 'dependency_sources': {'CrispEmbed': runtime_lock['source_commit']},
                 'artifacts': [{'filename': package.name, 'sha256': hashlib.sha256(package.read_bytes()).hexdigest()}]}
        receipt = self.app / 'CrispSorter_0.15.1_windows-x86_64.receipt.json'
        checksums = self.app / 'CrispSorter_0.15.1_windows-x86_64.sha256'
        checksums.write_text(value['artifacts'][0]['sha256'] + '  ' + package.name + '\n')
        original = json.dumps(value).encode(); receipt.write_bytes(original)
        with patch.object(retry, 'committed', side_effect=self.committed): retry.seal(self.app, proof, receipt)
        sealed = json.loads(receipt.read_text())
        self.assertEqual(sealed['app_source'], SOURCE)
        self.assertEqual(sealed['workflow_source'], 'a' * 40)
        self.assertEqual(sealed['artifacts'], value['artifacts'])
        self.assertEqual(sealed['windows_original_retry']['cargo_lock']['original_sha256'], self.pin['files']['Cargo.lock'])
        self.assertEqual(sealed['windows_original_retry']['cargo_lock']['effective_sha256'], self.pin['post_build_cargo_lock']['effective_sha256'])
        self.assertTrue(sealed['windows_original_retry']['cargo_lock']['lock_metadata_graph_equal'])
        self.assertEqual(sealed['windows_original_retry']['verified_receipt_sha256'], hashlib.sha256(original).hexdigest())
        for case in ('source', 'runtime', 'platform', 'package', 'checksum'):
            changed = copy.deepcopy(value)
            if case == 'source': changed['app_source'] = 'c' * 40
            elif case == 'runtime': changed['runtime_lock'] = {}
            elif case == 'platform': changed['platform'] = 'ios-arm64'
            elif case == 'package': changed['artifacts'][0]['sha256'] = '0' * 64
            else: checksums.write_text('unrelated checksum')
            receipt.write_text(json.dumps(changed)); before = receipt.read_bytes()
            with self.subTest(case=case), patch.object(retry, 'committed', side_effect=self.committed):
                with self.assertRaises(ValueError): retry.seal(self.app, proof, receipt)
                self.assertEqual(receipt.read_bytes(), before)
        with patch.object(retry, 'committed', side_effect=self.committed):
            with self.assertRaisesRegex(ValueError, 'exact Windows receipt'):
                retry.seal(self.app, proof, self.app / 'CrispSorter_0.15.1_ios-arm64.receipt.json')

    def test_upload_guard_refuses_unrelated_names_locations_and_symlinks(self):
        proof = self.verify()
        for name in ('unrelated.zip', 'CrispSorter_0.15.0_x64-windows-portable.zip',
                     'CrispSorter_0.15.1_ios-arm64.ipa', 'old-installer.exe'):
            package = self.app / name; package.write_bytes(b'fixture')
            with self.subTest(name=name), self.assertRaises(ValueError):
                retry.package_identity(self.app, proof, package)
        outside = self.app.parent / 'CrispSorter_0.15.1_x64-windows-portable.zip'
        outside.write_bytes(b'fixture')
        with self.assertRaises(ValueError): retry.package_identity(self.app, proof, outside)
        package = self.app / outside.name
        try: package.symlink_to(outside)
        except OSError: return
        with self.assertRaises(ValueError): retry.package_identity(self.app, proof, package)

    def test_workflow_uses_old_app_checkout_in_preflight_and_windows_only_and_rechecks_mutations(self):
        workflow = (ROOT / '.github/workflows/release.yml').read_text()
        checkout = "ref: ${{ inputs.platform == 'windows_original_b739' && '" + SOURCE + "' || github.sha }}"
        self.assertEqual(workflow.count(checkout), 2)
        self.assertEqual(workflow.count('ref: ${{ github.workflow_sha }}'), 2)
        self.assertIn("inputs.platform == 'windows' || inputs.platform == 'windows_original_b739'", workflow)
        self.assertIn("env.CRISPSORTER_MOBILE_RELEASE_PROFILE != 'ocr-draft' && inputs.platform != 'windows_original_b739'", workflow)
        self.assertIn("--phase check", workflow)
        self.assertIn("--phase seal", workflow)
        retry_upload = workflow.split("if ('${{ inputs.platform }}' -eq 'windows_original_b739') {", 1)[1].split('          } else {', 1)[0]
        self.assertIn('--phase check --package "$zip"', retry_upload)
        self.assertNotIn('gh release delete', retry_upload)
        self.assertNotIn('gh release edit', retry_upload)
        appstore = workflow.split('  release-macos-appstore:', 1)[-1]
        self.assertIn("inputs.platform != 'windows_original_b739'", appstore)
        for job in ('release-android:', 'release-ios:', 'release-required:', 'release-macos:'):
            section = workflow.split('  ' + job, 1)[1].split('    permissions:', 1)[0]
            self.assertNotIn("inputs.platform == 'windows_original_b739'", section)

    def test_real_git_crlf_checkouts_preserve_source_identity_only_after_workflow_guard(self):
        root = Path(self.temp.name) / 'real-git'; root.mkdir()
        config = root / 'isolated-global.gitconfig'
        config.write_text('[core]\n\tautocrlf = true\n')
        env = {**os.environ, 'GIT_CONFIG_GLOBAL': str(config), 'GIT_CONFIG_NOSYSTEM': '1'}

        def git(*args):
            return subprocess.check_output(['git', *map(str, args)], env=env, stderr=subprocess.PIPE)

        def seed(path, files):
            path.mkdir()
            git('init', '-q', path)
            for name, data in files.items():
                target = path / name; target.parent.mkdir(parents=True, exist_ok=True); target.write_bytes(data)
            git('-C', path, '-c', 'core.autocrlf=false', 'add', '.')
            git('-C', path, '-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid',
                '-c', 'core.autocrlf=false', 'commit', '-qm', 'LF source fixture')

        app_seed, tool_seed = root / 'app-seed', root / 'tool-seed'
        seed(app_seed, self.data)
        pin = copy.deepcopy(self.pin)
        pin['source_commit'] = git('-C', app_seed, 'rev-parse', 'HEAD').decode().strip()
        pin['source_tree'] = git('-C', app_seed, 'rev-parse', 'HEAD^{tree}').decode().strip()
        seed(tool_seed, {**self.data, '.github/workflows/release.yml': self.workflow,
                         retry.PIN: (json.dumps(pin, indent=2) + '\n').encode()})
        workflow_sha = git('-C', tool_seed, 'rev-parse', 'HEAD').decode().strip()
        bad_app, bad_tool = root / 'app-crlf', root / 'tool-crlf'
        for source, destination in ((app_seed, bad_app), (tool_seed, bad_tool)):
            git('clone', '-q', '--no-hardlinks', source, destination)
        self.assertIn(b'\r\n', (bad_tool / retry.PIN).read_bytes())
        event = {**self.env, 'GITHUB_WORKFLOW_SHA': workflow_sha}
        with patch.dict(os.environ, {key: env[key] for key in ('GIT_CONFIG_GLOBAL', 'GIT_CONFIG_NOSYSTEM')}), \
                patch.object(retry, 'lookup_draft'):
            with self.assertRaisesRegex(ValueError, 'committed bytes'):
                retry.verify(bad_app, bad_tool, 'Example/Project', event)
            steps = (ROOT / '.github/workflows/release.yml').read_text().split('    steps: &desktop_release_steps\n', 1)[1]
            guard, _ = steps.split('      - uses: actions/checkout@v4', 1)
            self.assertIn("if: runner.os == 'Windows'", guard)
            command = re.search(r'        run: (git config --global core.autocrlf false)', guard)[1]
            subprocess.run(shlex.split(command), env=env, check=True, capture_output=True)
            good_app, good_tool = root / 'app-lf', root / 'tool-lf'
            for source, destination in ((app_seed, good_app), (tool_seed, good_tool)):
                git('clone', '-q', '--no-hardlinks', source, destination)
            with self.assertRaisesRegex(ValueError, 'clean tracked|version/dependency'):
                retry.verify(bad_app, good_tool, 'Example/Project', event)
            proof = retry.verify(good_app, good_tool, 'Example/Project', event)
            self.assertEqual(proof['app_source'], pin['source_commit'])
            self.assertEqual(proof['workflow_source'], workflow_sha)
            for name in pin['files']:
                self.assertEqual((good_app / name).read_bytes(), retry.committed(good_app, name))
            self.assertEqual((good_tool / retry.PIN).read_bytes(), retry.committed(good_tool, retry.PIN))


if __name__ == '__main__': unittest.main()
