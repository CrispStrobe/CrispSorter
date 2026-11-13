#!/usr/bin/env python3
"""Version-consistency regressions without dependencies, builds or GitHub writes."""
import json
import os
import shutil
import subprocess
import textwrap
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

    def run_preflight(self, mode, event='workflow_dispatch', ref='main'):
        source = Path(__file__).resolve().parents[1]
        (self.root / 'scripts').mkdir(exist_ok=True)
        shutil.copyfile(source / 'scripts/check_release_versions.py', self.root / 'scripts/check_release_versions.py')
        binary = self.root / 'bin'; binary.mkdir(exist_ok=True)
        fake = binary / 'gh'
        fake.write_text("""#!/usr/bin/env python3
import json,os,sys
from pathlib import Path
args=sys.argv[1:]
with Path(os.environ['TEST_CALLS']).open('a') as stream:stream.write(json.dumps(args)+'\\n')
mode=os.environ['TEST_DRAFT_MODE']
if args[:2]==['release','view']:
 if mode in ['draft','public']:
  print(json.dumps({'isDraft':mode=='draft','targetCommitish':'older-workflow-source'}));sys.exit(0)
 sys.exit(1)
if args[0]=='api':
 print('HTTP 404' if mode=='missing' else 'HTTP 401',file=sys.stderr);sys.exit(1)
if args[:2]==['release','create']:sys.exit(0)
raise SystemExit('Unexpected mock GitHub command')
""")
        fake.chmod(0o755)
        git = binary / 'git'; git.write_text('#!/bin/sh\necho newer-workflow-source\n');git.chmod(0o755)
        workflow=(source / '.github/workflows/release.yml').read_text()
        section=workflow.split('      - name: Require a draft release before uploading artifacts',1)[1].split('  release-required:',1)[0]
        command=textwrap.dedent(section.split('        run: |\n',1)[1])
        calls=self.root / 'calls.jsonl';calls.unlink(missing_ok=True)
        result=subprocess.run(['bash','-c',command],cwd=self.root,env={**os.environ,
            'PATH':str(binary)+os.pathsep+os.environ['PATH'],'GITHUB_REPOSITORY':'Example/Project',
            'GITHUB_EVENT_NAME':event,'GITHUB_REF_NAME':ref,'RUNNER_TEMP':str(self.root),
            'TEST_CALLS':str(calls),'TEST_DRAFT_MODE':mode},capture_output=True,text=True,timeout=15)
        return result,[json.loads(line) for line in calls.read_text().splitlines()] if calls.exists() else []

    def test_same_version_retry_accepts_existing_draft_after_workflow_changes(self):
        result,calls=self.run_preflight('draft')
        self.assertEqual(result.returncode,0,result.stderr)
        self.assertEqual(len(calls),1)
        self.assertEqual(calls[0][:2],['release','view'])

    def test_public_release_and_lookup_errors_never_create_or_mutate_a_release(self):
        for mode in ['public','api-error']:
            result,calls=self.run_preflight(mode)
            self.assertNotEqual(result.returncode,0)
            self.assertFalse(any(call[:2]==['release','create'] for call in calls))

    def test_absent_release_creates_a_versioned_draft(self):
        result,calls=self.run_preflight('missing')
        self.assertEqual(result.returncode,0,result.stderr)
        create=calls[-1];self.assertEqual(create[:3],['release','create','v0.15.1'])
        self.assertIn('--draft',create)

    def test_tag_mismatch_stops_before_github_access(self):
        result,calls=self.run_preflight('draft',event='push',ref='v0.15.0')
        self.assertNotEqual(result.returncode,0)
        self.assertEqual(calls,[])


if __name__ == '__main__':
    unittest.main()
