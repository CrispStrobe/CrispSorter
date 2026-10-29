import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).resolve().parents[2] / 'scripts' / 'validate_android_native_libs.py'
spec = importlib.util.spec_from_file_location('validate_android_native_libs', SCRIPT)
validator = importlib.util.module_from_spec(spec)
spec.loader.exec_module(validator)


def elf(machine, *needed):
    lines = ['  Type: DYN (Shared object file)', f'  Machine: {machine}']
    lines += [f'  0x1 (NEEDED) Shared library: [{name}]' for name in needed]
    return '\n'.join(lines)


class ClosureTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.outputs = {}

    def library(self, name, machine='AArch64', needed=()):
        (self.root / name).touch()
        self.outputs[name] = elf(machine, *needed)

    def run_readelf(self, argv, **_kwargs):
        class Output:
            stdout = self.outputs[Path(argv[-1]).name]
        return Output()

    def test_closure_includes_transitive_dependencies_but_not_system(self):
        self.library('libcrispembed.so', needed=('libggml.so', 'libc.so'))
        self.library('libggml.so', needed=('libggml-base.so',))
        self.library('libggml-base.so')
        with patch.object(validator.subprocess, 'run', self.run_readelf):
            self.assertEqual(validator.closure(self.root, 'arm64-v8a'),
                             ['libcrispembed.so', 'libggml-base.so', 'libggml.so'])

    def test_missing_or_wrong_arch_dependency_is_rejected(self):
        self.library('libcrispembed.so', needed=('libggml.so',))
        with patch.object(validator.subprocess, 'run', self.run_readelf):
            with self.assertRaisesRegex(ValueError, 'missing Android native dependency'):
                validator.closure(self.root, 'arm64-v8a')
            self.library('libggml.so', machine='Advanced Micro Devices X86-64')
            with self.assertRaisesRegex(ValueError, 'not an Android arm64-v8a'):
                validator.closure(self.root, 'arm64-v8a')

    def test_dependency_path_is_rejected(self):
        self.library('libcrispembed.so', needed=('../libggml.so',))
        with patch.object(validator.subprocess, 'run', self.run_readelf):
            with self.assertRaisesRegex(ValueError, 'invalid dependency name'):
                validator.closure(self.root, 'arm64-v8a')


if __name__ == '__main__':
    unittest.main()
