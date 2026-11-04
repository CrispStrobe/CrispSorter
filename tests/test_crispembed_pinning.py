#!/usr/bin/env python3
"""Offline regression checks for CrispEmbed source/native release parity."""
import pathlib
import os
import re
import shutil
import subprocess
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[1]
TAG = 'v0.17.12'
COMMIT = '78493a31fc3043f3af4ab6fd2f197a32da6f07f0'


def text(path):
    return (ROOT / path).read_text(encoding='utf-8')


class CrispEmbedPinning(unittest.TestCase):
    def test_workflows_pin_source_commit_and_release_tag(self):
        for name in ('.github/workflows/ci.yml', '.github/workflows/release.yml'):
            with self.subTest(name=name):
                s = text(name)
                self.assertRegex(s, rf'(?m)^  CRISPEMBED_REF: {TAG}\b')
                self.assertRegex(s, rf'(?m)^  CRISPEMBED_COMMIT: {COMMIT}\b')
                checkouts = re.findall(r'repository: \$\{\{ env\.CRISPEMBED_REPO \}\}\s*\n\s*ref: ([^\n]+)', s)
                self.assertTrue(checkouts)
                self.assertTrue(all(x.strip() == '${{ env.CRISPEMBED_COMMIT }}' for x in checkouts), checkouts)
                self.assertNotRegex(s, r'github\.com/\$\{\{ env\.CRISPEMBED_REPO \}\}/releases/(latest|download/(?!\$\{\{ env\.CRISPEMBED_REF \}\}))')

    def test_helpers_refuse_unverified_source_and_binary_paths(self):
        sh = text('enable-crispembed.sh')
        ps = text('enable-crispembed.ps1')
        for s in (sh, ps):
            self.assertIn(TAG, s)
            self.assertIn(COMMIT, s)
            self.assertNotIn('release download --pattern', s)
            self.assertIn('rev-parse HEAD', s)
            self.assertIn('status --porcelain', s)
        self.assertRegex(sh, r'gh release download "\$CRISPEMBED_REF"')
        self.assertRegex(ps, r'& \$GH_EXE release download \$CrispEmbedRef')
        self.assertIn('sha256', sh.lower())
        self.assertIn('sha256', ps.lower())

    def test_release_archive_digests_match_each_recipe(self):
        # A lockfile left at the previous release makes `cargo build --locked`
        # against the pinned checkout fail, so the two must move together.
        lock = text('Cargo.lock')
        pinned = TAG.lstrip('v')
        for crate in ('crispembed', 'crispembed-sys'):
            with self.subTest(crate=crate):
                self.assertRegex(
                    lock,
                    rf'\[\[package\]\]\nname = "{crate}"\nversion = "{re.escape(pinned)}"',
                )

        ci = text('.github/workflows/ci.yml')
        rel = text('.github/workflows/release.yml')
        mac = '6bdbf65587eac063ec6658d9e03345a63614cee680ea887ef51eaabcc72f7a56'
        linux = '1e53efd8d78e6f6abeed252e3f715c5c34c8b97508922c5983285400c96e2b63'
        windows = '49a1e4a89bf6f7d2df100794914f15aeb9a2b69513c3dbfb93364208994cc717'
        for digest in (mac, linux):
            self.assertIn(digest, ci)
        for digest in (mac, linux, windows):
            self.assertIn(digest, rel)
        self.assertIn('matrix.crispembed_sha256', rel)
        for asset, digest in (
            ('crispembed-macos-arm64.tar.gz', mac),
            ('crispembed-linux-x86_64.tar.gz', linux),
            ('crispembed-windows-x86_64.zip', windows),
        ):
            self.assertIn(f'crispembed_lib_asset: {asset}\n            crispembed_sha256: {digest}', rel)

    def test_shell_rejects_wrong_dirty_and_tampered_inputs(self):
        with tempfile.TemporaryDirectory() as tmp:
            base = pathlib.Path(tmp)
            project = base / 'CrispSorter'
            project.mkdir()
            shutil.copy(ROOT / 'enable-crispembed.sh', project)
            sibling = base / 'CrispEmbed' / 'crispembed'
            sibling.mkdir(parents=True)
            (sibling / 'Cargo.toml').write_text('fixture')
            fakebin = base / 'bin'
            fakebin.mkdir()
            git = fakebin / 'git'
            git.write_text('#!/bin/sh\ncase "$3" in rev-parse) printf "%s\\n" "$TEST_COMMIT";; status) printf "%s" "$TEST_DIRTY";; *) exit 1;; esac\n')
            git.chmod(0o755)
            uname = fakebin / 'uname'
            uname.write_text('#!/bin/sh\nif [ "$1" = -s ]; then echo Linux; else echo x86_64; fi\n')
            uname.chmod(0o755)
            env = dict(os.environ, PATH=str(fakebin) + os.pathsep + os.environ['PATH'], TEST_COMMIT='bad', TEST_DIRTY='')

            def run():
                return subprocess.run(['bash', str(project / 'enable-crispembed.sh'), '--skip-download', '--dry-run'], env=env, text=True, capture_output=True)

            result = run()
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('source must be clean', result.stderr)
            env['TEST_COMMIT'], env['TEST_DIRTY'] = COMMIT, ' M changed'
            result = run()
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('source must be clean', result.stderr)
            env['TEST_DIRTY'] = ''
            result = run()
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('Pinned archive missing', result.stderr)
            archive = project / 'src-tauri/crispembed-prebuilt-cache' / TAG / 'crispembed-linux-x86_64.tar.gz'
            archive.write_bytes(b'not the release archive')
            result = run()
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('SHA256 mismatch', result.stderr)

            # Optional integration check against the actual published archive;
            # no network access or weights are needed in the normal CI test.
            real_archive = os.environ.get('CRISPEMBED_TEST_ARCHIVE')
            if real_archive:
                shutil.copyfile(real_archive, archive)
                env['CARGO_TARGET_DIR'] = str(base / 'target')
                result = run()
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertIn('DryRun: stopping', result.stdout)
                self.assertTrue((base / 'target/debug/libcrispembed.so').is_file())


if __name__ == '__main__':
    unittest.main()
