import importlib.util
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'scripts'))
spec = importlib.util.spec_from_file_location('mobile_crispembed', ROOT / 'scripts' / 'build_mobile_crispembed.py')
mobile = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mobile)


class BuildTests(unittest.TestCase):
    def test_configured_source_and_submodule_match_without_clean_tree_gate(self):
        lock = {'source_commit': 'source', 'ggml_commit': 'ggml'}
        source = Path('/checkout')
        def fake_git(repo, *args):
            self.assertEqual(args, ('rev-parse', 'HEAD'))
            return 'ggml' if repo == source / 'ggml' else 'source'
        with patch.object(mobile, 'git', side_effect=fake_git): mobile.check_source(source, lock)
        with patch.object(mobile, 'git', return_value='wrong'):
            with self.assertRaisesRegex(ValueError, 'does not match'): mobile.check_source(source, lock)

    def test_ios_requires_complete_archives(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            build, libdir = root / 'build', root / 'lib'
            (build / 'ggml').mkdir(parents=True)
            libdir.mkdir()
            for name in ('libcrispembed-static.a', 'libcrispembed-core.a'):
                (build / name).touch()
            for name in ('libggml.a', 'libggml-base.a', 'libggml-cpu.a'):
                (build / 'ggml' / name).touch()
            with self.assertRaisesRegex(ValueError, 'libggml-metal.a'):
                mobile.package_ios(build, libdir)
            (build / 'ggml/libggml-metal.a').touch()
            with patch.object(mobile, 'run') as run:
                self.assertEqual(mobile.package_ios(build, libdir), [libdir / 'libcrispembed.a'])
                self.assertIn(build / 'libcrispembed-core.a', run.call_args.args[0])
                self.assertIn(build / 'ggml/libggml-metal.a', run.call_args.args[0])

    def test_rerun_rebuilds_and_revalidates_without_clearing_unrelated_output(self):
        import json
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp); source = root / 'source'; source.mkdir()
            output = root / 'output'; output.mkdir()
            marker = output / 'keep.txt'; marker.write_text('unrelated')
            lock = root / 'lock.json'; lock.write_text('{"source_commit":"source","ggml_commit":"ggml"}')
            def run(args, **kwargs):
                if args[:2] == ['cmake', '--build']:
                    (output / 'build/libcrispembed.so').write_bytes(b'new native runtime')
            with patch.object(mobile, 'check_source') as check, patch.object(mobile, 'configure_args', return_value=['cmake']), \
                    patch.object(mobile, 'run', side_effect=run), patch.object(mobile, 'closure', return_value=['libcrispembed.so']) as closure, \
                    patch.object(mobile, 'collect_native_provenance', return_value={}) as notices:
                for _ in range(2): mobile.build_runtime(source, output, 'android-arm64', 1, None, lock)
                self.assertEqual(check.call_count, 4); self.assertEqual(closure.call_count, 2); self.assertEqual(notices.call_count, 2)
            self.assertEqual(marker.read_text(), 'unrelated')
            self.assertEqual((output / 'lib/libcrispembed.so').read_bytes(), b'new native runtime')
            with patch.object(mobile, 'check_source') as check:
                with self.assertRaisesRegex(ValueError, 'outside the source'):
                    mobile.build_runtime(source, source / 'build', 'android-arm64', 1, None, lock)
                check.assert_not_called()

    def test_android_disables_unpinned_optional_siblings(self):
        with tempfile.TemporaryDirectory() as temp:
            ndk = Path(temp)
            toolchain = ndk / 'build/cmake/android.toolchain.cmake'
            toolchain.parent.mkdir(parents=True)
            toolchain.touch()
            clang = ndk / 'toolchains/llvm/prebuilt/linux-x86_64/bin/clang'
            clang.parent.mkdir(parents=True)
            clang.touch()
            args = mobile.configure_args('android-arm64', Path('/source'), Path('/out'), ndk)
            for component in ('AUDIO', 'PUNC', 'LID', 'TRUECASE'):
                self.assertIn(f'-DCRISP_{component}_DIR=/out/disabled-optional/{component}', args)
            self.assertIn('-DBUILD_SHARED_LIBS=OFF', args)
            self.assertIn('-DANDROID_ABI=arm64-v8a', args)
            self.assertIn('-DANDROID_SUPPORT_FLEXIBLE_PAGE_SIZES=ON', args)
            simulator_args = mobile.configure_args('android-x86_64', Path('/source'), Path('/out'), ndk)
            self.assertIn('-DANDROID_ABI=x86_64', simulator_args)
            self.assertNotIn('-DANDROID_ABI=arm64-v8a', simulator_args)
            for flag in ('-DANDROID_NATIVE_API_LEVEL=24', '-DANDROID_STL=c++_static',
                         '-DCRISPEMBED_BUILD_SHARED=ON', '-DGGML_NATIVE=OFF',
                         '-DCRISPEMBED_NATIVE=OFF', '-DGGML_METAL=OFF',
                         '-DANDROID_SUPPORT_FLEXIBLE_PAGE_SIZES=ON'):
                self.assertIn(flag, simulator_args)
            armv7_args = mobile.configure_args('android-armv7', Path('/source'), Path('/out'), ndk)
            self.assertIn('-DANDROID_ABI=armeabi-v7a', armv7_args)
            self.assertIn('-DANDROID_NATIVE_API_LEVEL=24', armv7_args)
            self.assertIn('-DANDROID_STL=c++_static', armv7_args)
            self.assertIn('-DCRISPEMBED_BUILD_SHARED=ON', armv7_args)

    def test_unknown_target_never_falls_through_to_ios(self):
        with patch.object(mobile, 'run') as run:
            with self.assertRaisesRegex(ValueError, 'unsupported mobile target'):
                mobile.configure_args('host-linux', Path('/source'), Path('/out'))
            run.assert_not_called()

    def test_explicit_feature_lock_is_checked_before_native_configuration(self):
        import json
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            source = root / 'source'
            source.mkdir()
            lock_path = root / 'feature-lock.json'
            lock = {'source_commit': 'feature-source', 'ggml_commit': 'feature-ggml'}
            lock_path.write_text(json.dumps(lock))
            with patch.object(mobile, 'check_source', side_effect=ValueError('source mismatch')) as check:
                with patch.object(mobile, 'configure_args') as configure:
                    with self.assertRaisesRegex(ValueError, 'source mismatch'):
                        mobile.build_runtime(source, root / 'output', 'android-arm64', 1, None, lock_path)
                    check.assert_called_once_with(source.resolve(), lock)
                    configure.assert_not_called()


    def test_rejects_incomplete_ndk_before_configure(self):
        with tempfile.TemporaryDirectory() as temp:
            with self.assertRaisesRegex(ValueError, 'NDK is incomplete'):
                mobile.configure_args('android-arm64', Path('/source'), Path('/out'), Path(temp))


if __name__ == '__main__':
    unittest.main()
