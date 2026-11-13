"""Fresh desktop packages provision real OCR prerequisites before the OCR probe."""
import json
import os
from pathlib import Path
import plistlib
import subprocess
import tarfile
import tempfile
import unittest
from unittest.mock import patch
import zipfile

import verify_release_artifacts as verifier


class DesktopModels(unittest.TestCase):
    def test_every_desktop_provisions_before_real_ocr(self):
        for platform in ('windows-x86_64', 'macos-arm64', 'linux-x86_64'):
            with self.subTest(platform=platform), tempfile.TemporaryDirectory() as work:
                root = Path(work)
                source = root / 'source'; source.mkdir()
                app = source / 'CrispSorter.app'
                (app / 'Contents/MacOS').mkdir(parents=True)
                (app / 'Contents/MacOS/crispsorter').write_bytes(b'executable')
                (app / 'Contents/Info.plist').write_bytes(plistlib.dumps({'CFBundleExecutable': 'crispsorter'}))
                (source / 'crispsorter.exe').write_bytes(b'executable')
                artifact = root / ('package.zip' if platform.startswith('windows') else 'package.tar.gz' if platform.startswith('macos') else 'package.deb')
                if artifact.suffix == '.zip':
                    with zipfile.ZipFile(artifact, 'w') as archive:
                        archive.write(source / 'crispsorter.exe', 'crispsorter.exe')
                elif artifact.name.endswith('.tar.gz'):
                    with tarfile.open(artifact, 'w:gz') as archive:
                        archive.add(app, arcname=app.name)
                else: artifact.write_bytes(b'deb fixture')
                destination = root / 'unpacked'; destination.mkdir()
                state = {'installed': False, 'ocr': False, 'pulled': False}
                def plan():
                    return {'engine': 'dbnet_trocr', 'missing': [] if state['installed'] else ['dbnet-det', 'trocr-printed'],
                            'models': [{'name': name, 'license': license, 'requires_consent': False, 'installed': state['installed']}
                                       for name, license in [('dbnet-det', 'apache-2.0'), ('trocr-printed', 'mit')]]}
                def run(command, **kwargs):
                    if command[0] == 'dpkg-deb':
                        (destination / 'usr/bin').mkdir(parents=True)
                        (destination / 'usr/bin/crispsorter').write_bytes(b'executable')
                        output = ''
                    elif command[0] == 'tar':
                        with tarfile.open(artifact) as archive: archive.extractall(destination, filter='data')
                        output = ''
                    elif 'models' in command:
                        self.assertNotIn('--accept-license', command)
                        if 'pull' in command:
                            state.update(installed=True, pulled=True)
                        output = json.dumps(plan())
                    elif 'ocr' in command:
                        self.assertTrue(kwargs.get('check'), 'packaged OCR failure must remain fatal')
                        self.assertTrue(state['installed'], 'fresh-cache OCR ran before explicit model provisioning')
                        state['ocr'] = True
                        output = 'ocr_page cobalt warranty 2031'
                    else: output = '{}'
                    return subprocess.CompletedProcess(command, 0, output, '')
                with patch.object(verifier.subprocess, 'run', side_effect=run), \
                        patch.object(verifier.subprocess, 'check_output', return_value='Requesting program interpreter: /lib/ld.so]'), \
                        patch.object(verifier, 'check_ocr_worker', return_value={}), \
                        patch.dict(os.environ, {'CRISPSORTER_REQUIRE_DESKTOP_SIGNING': '0'}):
                    verifier.inspect_package(platform, artifact, destination)
                self.assertTrue(state['pulled'])
                self.assertTrue(state['ocr'])

    def test_failure_diagnostics_are_bounded_and_safe(self):
        for secret in ('{"token":"SECRET-SENTINEL"}', 'https://user:SECRET-SENTINEL@host/x', 'https://host/x?key=SECRET-SENTINEL', 'Bearer SECRET-SENTINEL'):
            error = subprocess.CalledProcessError(1, ['app'], output='', stderr=secret)
            with patch.object(verifier.subprocess, 'run', side_effect=error):
                with self.assertRaises(RuntimeError) as caught:
                    verifier.run_packaged(['app'], Path('/tmp'), {}, 10)
            self.assertNotIn('SECRET-SENTINEL', str(caught.exception))
            self.assertIn('exit 1', str(caught.exception))
        for path in (r'C:\Users\private\temp\SECRET-SENTINEL', r'\\server\private\SECRET-SENTINEL', r'C:/private/SECRET-SENTINEL'):
            with patch.object(verifier.subprocess, 'run', side_effect=subprocess.CalledProcessError(1, ['app'], stderr=path)):
                with self.assertRaises(RuntimeError) as caught:
                    verifier.run_packaged(['app'], Path('/tmp'), {}, 10)
            self.assertNotIn('SECRET-SENTINEL', str(caught.exception))
            self.assertIn('Windows path content', str(caught.exception))
        for error in (subprocess.CalledProcessError(1, ['app'], stderr='missing OCR models dbnet-det'),
                      subprocess.TimeoutExpired(['app'], 1, stderr=b'model download stalled')):
            with patch.object(verifier.subprocess, 'run', side_effect=error):
                with self.assertRaises(RuntimeError) as caught:
                    verifier.run_packaged(['app'], Path('/tmp'), {}, 10)
            self.assertIn('models' if isinstance(error, subprocess.CalledProcessError) else 'stalled', str(caught.exception))
        with patch.object(verifier.subprocess, 'run', side_effect=subprocess.CalledProcessError(1, ['app'], stderr='x' * 300000)):
            with self.assertRaises(RuntimeError) as caught:
                verifier.run_packaged(['app'], Path('/tmp'), {}, 10)
        self.assertLess(len(str(caught.exception)), 8500)


if __name__ == '__main__': unittest.main()
