import copy
from pathlib import Path
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch
import zipfile

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'scripts'))
import mobile_native_provenance as provenance


class NativeProvenanceTests(unittest.TestCase):
    def fixture(self, root, sheen=True, platform='android'):
        source, output = root / 'source', root / 'runtime'
        source.mkdir()
        (source / 'ggml').mkdir()
        notices = dict(provenance.BASE_NOTICES)
        if sheen:
            notices.update(provenance.SHEEN_NOTICES)
        for name, (repository, relative) in notices.items():
            checkout = source if repository == 'crispembed' else source / 'ggml'
            path = checkout / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(('commit `' + 'a' * 40 + '`\n') if name == 'SheenBidi-UPSTREAM.md' else 'self-authored notice fixture for ' + name + '\n')
        if sheen:
            (source / 'vendor/sheenbidi/Source').mkdir()
            (source / 'vendor/sheenbidi/Source/SheenBidi.c').write_text('// self-authored source fixture; never compiled\n')
        for checkout in (source / 'ggml', source):
            for args in (['init', '-q'], ['add', '.'],
                         ['-c', 'user.name=Provenance control', '-c', 'user.email=provenance-control@example.invalid', 'commit', '-qm', 'fixture']):
                subprocess.run(['git', '-C', str(checkout), *args], check=True, capture_output=True, timeout=30)
        (output / 'lib').mkdir(parents=True)
        library = output / 'lib' / ('libcrispembed.so' if platform == 'android' else 'libcrispembed.a')
        library.write_bytes(b'self-authored artifact identity fixture; never executed')
        lock = {'native_components': {'sheenbidi': {'source_tree': provenance.git(source, 'rev-parse', 'HEAD:vendor/sheenbidi')}}} if sheen else {}
        return source, output, library, lock

    def collect(self, source, output, library, lock, platform='android'):
        real_run = subprocess.run
        def run(command, **kwargs):
            if (command[:2] == ['xcrun', 'nm'] or command[0].endswith('/llvm-nm')):
                prefix = '_' if command[0] == 'xcrun' else ''
                return SimpleNamespace(stdout='\n'.join('0000000010 T ' + prefix + symbol for symbol in provenance.SHEEN_SYMBOLS))
            return real_run(command, **kwargs)
        with patch.object(provenance.subprocess, 'run', side_effect=run):
            proof = provenance.collect(source, output, 'android-x86_64' if platform == 'android' else 'ios-sim-arm64',
                                       [library], lock, Path('/fixture-ndk'))
        return {'artifacts': {library.name: provenance.file_sha256(library)}, 'native_provenance': proof}

    def package(self, root, output, platform='android', missing=None, changed=None, duplicate=None):
        if platform == 'android':
            artifact = root / 'app.apk'
            with zipfile.ZipFile(artifact, 'w') as archive:
                for notice in (output / 'notices').iterdir():
                    if notice.name != missing:
                        archive.writestr('assets/bin/' + notice.name, b'changed' if notice.name == changed else notice.read_bytes())
                    if notice.name == duplicate:
                        archive.writestr('assets/extra/' + notice.name, notice.read_bytes())
        else:
            artifact = root / 'CrispSorter.app'
            (artifact / 'bin').mkdir(parents=True)
            for notice in (output / 'notices').iterdir():
                (artifact / 'bin' / notice.name).write_bytes(notice.read_bytes())
        return artifact

    def test_source_notice_blobs_and_vendor_tree_survive_android_packaging(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, output, library, lock = self.fixture(root)
            runtime = self.collect(source, output, library, lock)
            artifact = self.package(root, output)
            result = provenance.verify_packaged(artifact, 'android', output, runtime, lock)
            self.assertEqual(len(result['packaged_notices']), 10)
            component = result['components']['sheenbidi']
            self.assertEqual(component['source_tree'], lock['native_components']['sheenbidi']['source_tree'])
            self.assertEqual(component['artifacts'][library.name]['defined_symbols'], list(provenance.SHEEN_SYMBOLS))

    def test_old_source_without_vendor_still_carries_existing_native_notices(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, output, library, lock = self.fixture(root, sheen=False)
            runtime = self.collect(source, output, library, lock)
            result = provenance.verify_packaged(self.package(root, output), 'android', output, runtime, lock)
            self.assertEqual(result['components'], {})
            self.assertEqual(len(result['packaged_notices']), 7)

    def test_required_vendor_must_match_immutable_source_tree(self):
        for sheen in (False, True):
            with self.subTest(sheen=sheen), tempfile.TemporaryDirectory() as directory:
                source, output, library, lock = self.fixture(Path(directory), sheen=sheen)
                lock = {'native_components': {'sheenbidi': {'source_tree': 'f' * 40}}}
                with self.assertRaisesRegex(ValueError, 'source tree differs'):
                    self.collect(source, output, library, lock)

    def test_missing_or_undefined_symbols_do_not_count_as_embedded_implementation(self):
        complete = ['00000010 T ' + symbol for symbol in provenance.SHEEN_SYMBOLS]
        for replacement in ('                 U SBAlgorithmCreate', '00000010 T SBAlgorithmCreateExtra', 'member(SBAlgorithmCreate.o):'):
            with self.subTest(replacement=replacement), self.assertRaisesRegex(ValueError, 'does not define'):
                provenance.defined_sheen_symbols('\n'.join([replacement, *complete[1:]]))
        self.assertEqual(provenance.defined_sheen_symbols('\n'.join('00000010 T _' + symbol for symbol in provenance.SHEEN_SYMBOLS)), list(provenance.SHEEN_SYMBOLS))

    def test_changed_or_symlinked_source_notices_are_rejected(self):
        for mode in ('changed', 'symlink'):
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                source, output, library, lock = self.fixture(root)
                notice = source / 'vendor/sheenbidi/LICENSE'
                if mode == 'changed':
                    notice.write_text('uncommitted content')
                else:
                    notice.unlink()
                    notice.symlink_to(source / 'LICENSE')
                with self.assertRaises(ValueError):
                    self.collect(source, output, library, lock)

    def test_missing_changed_and_duplicate_app_notices_are_rejected(self):
        for mode in ('missing', 'changed', 'duplicate'):
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                source, output, library, lock = self.fixture(root)
                runtime = self.collect(source, output, library, lock)
                artifact = self.package(root, output, **{mode: 'SheenBidi-LICENSE.txt'})
                with self.assertRaisesRegex(ValueError, 'native notice'):
                    provenance.verify_packaged(artifact, 'android', output, runtime, lock)

    def test_omitted_component_or_changed_artifact_invalidates_symbol_proof(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, output, library, lock = self.fixture(root)
            runtime = self.collect(source, output, library, lock)
            artifact = self.package(root, output)
            for change in ('omit', 'symbol', 'hash'):
                modified = copy.deepcopy(runtime)
                component = modified['native_provenance']['components']['sheenbidi']
                if change == 'omit':
                    modified['native_provenance']['components'].clear()
                elif change == 'symbol':
                    component['artifacts'][library.name]['defined_symbols'].pop()
                else:
                    component['artifacts'][library.name]['sha256'] = '0' * 64
                with self.subTest(change=change), self.assertRaises(ValueError):
                    provenance.verify_packaged(artifact, 'android', output, modified, lock)
            library.write_bytes(b'different artifact')
            with self.assertRaisesRegex(ValueError, 'artifact changed'):
                provenance.verify_packaged(artifact, 'android', output, runtime, lock)

    def test_ios_flat_archive_and_actual_bundle_notice_identity(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, output, library, lock = self.fixture(root, platform='ios')
            runtime = self.collect(source, output, library, lock, platform='ios')
            artifact = self.package(root, output, platform='ios')
            result = provenance.verify_packaged(artifact, 'ios-simulator', output, runtime, lock)
            self.assertEqual(len(result['packaged_notices']), 10)
            notice = artifact / 'bin/SheenBidi-UPSTREAM.md'
            notice.unlink()
            notice.symlink_to(output / 'notices/SheenBidi-UPSTREAM.md')
            with self.assertRaisesRegex(ValueError, 'regular and source-contained'):
                provenance.verify_packaged(artifact, 'ios-simulator', output, runtime, lock)


if __name__ == '__main__':
    unittest.main()
