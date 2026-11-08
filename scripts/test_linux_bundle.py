#!/usr/bin/env python3
"""Cold-loader regression for Debian SONAME aliases, modes and ownership."""
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest


@unittest.skipUnless(sys.platform.startswith('linux'), 'Linux ELF/Debian packaging')
class LinuxBundleTest(unittest.TestCase):
    def test_installed_layout_resolves_its_own_soname_and_preserves_control_format(self):
        for program in ['cc', 'dpkg-deb', 'ar', 'patchelf', 'tar']:
            if not shutil.which(program):
                self.skipTest(program + ' is required')
        with tempfile.TemporaryDirectory(prefix='crispsorter-bundle-test-') as work:
            root = Path(work)
            libs = root / 'libs'
            libs.mkdir()
            source = root / 'probe.c'
            source.write_text('int crisp_packaging_probe_20261028(void) { return 42; }\n')
            subprocess.run(['cc', '-shared', '-fPIC', str(source), '-Wl,-soname,libcrispembed.so.0', '-o', str(libs / 'libcrispembed.so.0.1.0')], check=True)
            (libs / 'libcrispembed.so.0').symlink_to('libcrispembed.so.0.1.0')
            (libs / 'libcrispembed.so').symlink_to('libcrispembed.so.0')
            source.write_text('extern int crisp_packaging_probe_20261028(void); int main(void) { return crisp_packaging_probe_20261028() == 42 ? 0 : 1; }\n')
            package = root / 'package'
            (package / 'DEBIAN').mkdir(parents=True)
            (package / 'usr/bin').mkdir(parents=True)
            (package / 'DEBIAN/control').write_text('Package: crisp-bundle-fixture\nVersion: 0.0.1\nArchitecture: amd64\nMaintainer: CrispSorter qualification\nDescription: Disposable loader fixture\n')
            for directory in [package, *package.rglob('*')]:
                if directory.is_dir(): directory.chmod(0o755)
            (package / 'DEBIAN/control').chmod(0o644)
            subprocess.run(['cc', str(source), '-L' + str(libs), '-lcrispembed', '-o', str(package / 'usr/bin/probe')], check=True)
            (package / 'usr/bin/probe').chmod(0o755)
            env = os.environ.copy()
            env.update(CRISPEMBED_BUILD_DIR=str(libs), CRISPASR_BUILD_DIR=str(root / 'absent'), LD_LIBRARY_PATH='')
            script = Path(__file__).with_name('bundle_linux_native_libs.sh')
            for compression in ['gzip', 'xz']:
                with self.subTest(compression=compression):
                    bundle = root / (compression + '.deb')
                    subprocess.run(['dpkg-deb', '-Z' + compression, '-z1', '--build', str(package), str(bundle)], check=True, capture_output=True)
                    subprocess.run(['bash', str(script), str(bundle)], check=True, env=env, capture_output=True)
                    installed = root / compression
                    subprocess.run(['dpkg-deb', '-x', str(bundle), str(installed)], check=True)
                    subprocess.run([str(installed / 'usr/bin/probe')], env=env, check=True, capture_output=True)
                    self.assertTrue((installed / 'usr/lib/CrispSorter/native/libcrispembed.so.0').is_file())
                    # Inspect archive metadata, not the extracting user's umask.
                    data = root / (compression + '.tar')
                    with data.open('wb') as stream:
                        subprocess.run(['dpkg-deb', '--fsys-tarfile', str(bundle)], stdout=stream, check=True)
                    with tarfile.open(data) as archive:
                        for member in archive.getmembers():
                            self.assertEqual((member.uid, member.gid), (0, 0))
                            if member.name.endswith('/native') or member.name.endswith('/libcrispembed.so.0'):
                                self.assertEqual(member.mode & 0o777, 0o755)


if __name__ == '__main__':
    unittest.main()
