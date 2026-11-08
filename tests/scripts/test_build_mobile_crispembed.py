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
    def test_source_and_submodule_must_match_and_be_clean(self):
        lock = {'source_commit': 'source', 'ggml_commit': 'ggml'}
        source = Path('/checkout')
        values = {('rev-parse', 'HEAD'): 'source', ('status', '--porcelain', '--untracked-files=all'): ''}
        def fake_git(repo, *args):
            if repo == source / 'ggml' and args == ('rev-parse', 'HEAD'):
                return 'ggml'
            return values[args]
        with patch.object(mobile, 'git', side_effect=fake_git):
            mobile.check_source(source, lock)
            values[('status', '--porcelain', '--untracked-files=all')] = ' M source.cpp'
            with self.assertRaisesRegex(ValueError, 'must be clean'):
                mobile.check_source(source, lock)
        with patch.object(mobile, 'git', return_value='wrong'):
            with self.assertRaisesRegex(ValueError, 'does not match'):
                mobile.check_source(source, lock)

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

    def test_rejects_checkout_or_nonempty_output_before_running_build(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            source, output = root / 'source', root / 'output'
            source.mkdir()
            output.mkdir()
            (output / 'old').touch()
            with patch.object(mobile, 'check_source') as check:
                with self.assertRaisesRegex(ValueError, 'must be empty'):
                    mobile.build_runtime(source, output, 'android-arm64', 1, None)
                with self.assertRaisesRegex(ValueError, 'outside the source'):
                    mobile.build_runtime(source, source / 'build', 'android-arm64', 1, None)
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

    def test_rejects_incomplete_ndk_before_configure(self):
        with tempfile.TemporaryDirectory() as temp:
            with self.assertRaisesRegex(ValueError, 'NDK is incomplete'):
                mobile.configure_args('android-arm64', Path('/source'), Path('/out'), Path(temp))


if __name__ == '__main__':
    unittest.main()
