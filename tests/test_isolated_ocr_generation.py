#!/usr/bin/env python3
"""Execute OCR probe generation without Rust, native libraries or model downloads.

This checks generated source/dependency wiring; it is not native qualification.
"""
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import tomllib
import unittest


ROOT = Path(__file__).resolve().parents[1]
GENERATOR = ROOT / 'tools/quality/isolated_ocr.py'


class IsolatedOcrGeneration(unittest.TestCase):
    def test_basic_panel_and_worker_crates_generate_from_current_sources(self):
        locked = tomllib.loads((ROOT / 'Cargo.lock').read_text())['package']
        for switches in [[], ['--panel'], ['--worker'], ['--panel', '--worker']]:
            with self.subTest(switches=switches), tempfile.TemporaryDirectory(
                    prefix='ocr-generation-test-', dir=ROOT) as temporary:
                destination = Path(temporary)
                # Generation must not require a checkout of the native runtime.
                embed = destination / 'absent-native-runtime'
                result = subprocess.run([sys.executable, str(GENERATOR), '--app', str(ROOT),
                    '--embed', str(embed), '--destination', str(destination), *switches],
                    check=True, capture_output=True, text=True)
                receipt = json.loads(result.stdout)
                self.assertEqual(receipt, json.loads((destination / 'source-fragments.json').read_text()))
                self.assertIn('Generation only', receipt['native_execution'])
                self.assertFalse(embed.exists(), 'generation accessed or created the native runtime')
                for relative, digest in receipt['source_files_sha256'].items():
                    self.assertEqual(hashlib.sha256((ROOT / relative).read_bytes()).hexdigest(), digest, relative)

                source = (destination / 'lib.rs').read_text()
                modules = {name: Path(json.loads(path)) for path, name in re.findall(
                    r'#\[path = ("(?:[^"\\]|\\.)*")\] pub mod (\w+);', source)}
                for name in ['operation_log', 'ocr_word_evidence', 'license_consent',
                             'ocr_crispembed', 'ocr_policy', 'ocr_worker', 'page_source']:
                    self.assertIn(name, modules)
                    self.assertTrue(modules[name].is_file())
                    relative = modules[name].relative_to(ROOT).as_posix()
                    self.assertIn(relative, receipt['source_files_sha256'])
                # All three worker modes compile against the production image
                # dispatcher and review implementation, not inferred probe data.
                self.assertIn('fn ocr_image_page(', source)
                self.assertIn('fn prepare_ocr_page(', source)
                self.assertIn('pub fn app_log(', source)
                self.assertIn('pub fn resolve_model(', source)
                self.assertNotIn('fn cached_pipeline(', source)

                manifest = tomllib.loads((destination / 'Cargo.toml').read_text())
                dependencies = manifest['dependencies']
                for name in ['anyhow', 'serde', 'serde_json', 'sha2', 'tempfile', 'uuid',
                             'image', 'kamadak-exif', 'tiff', 'ocrs', 'rten']:
                    version = dependencies[name]['version']
                    self.assertTrue(version.startswith('='), name)
                    self.assertIn((name, version[1:]), {(p['name'], p['version']) for p in locked})
                self.assertEqual(dependencies['uuid']['features'], ['v4'])
                self.assertEqual(dependencies['crispembed']['path'], (embed / 'crispembed').as_posix())
                self.assertTrue(dependencies['crispembed']['optional'])
                self.assertEqual(dependencies['crispembed-sys']['path'], (embed / 'crispembed-sys').as_posix())
                self.assertTrue(dependencies['crispembed-sys']['optional'])
                self.assertEqual(manifest['features']['crispembed'], ['dep:crispembed', 'dep:crispembed-sys'])
                self.assertIn('sidecars', manifest['features']['default'])
                self.assertIn('desktop-mas', manifest['features'])
                binaries = {binary['name'] for binary in manifest.get('bin', [])}
                self.assertEqual('ocr-case' in binaries, '--panel' in switches)
                self.assertEqual('ocr-worker-probe' in binaries, '--worker' in switches)
                for binary in manifest.get('bin', []):
                    self.assertTrue((destination / binary['path']).is_file())
                self.assertEqual((destination / 'Cargo.lock').read_bytes(), (ROOT / 'Cargo.lock').read_bytes())

    def test_destination_outside_fixture_parent_is_rejected_before_writing(self):
        with tempfile.TemporaryDirectory(prefix='ocr-generation-parent-', dir=ROOT) as temporary:
            destination = Path(temporary) / 'nested'
            result = subprocess.run([sys.executable, str(GENERATOR), '--app', str(ROOT),
                '--embed', str(ROOT / 'absent-runtime'), '--destination', str(destination)],
                capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('Probe must sit beside tests/', result.stderr)
            self.assertFalse(destination.exists())


if __name__ == '__main__':
    unittest.main()
