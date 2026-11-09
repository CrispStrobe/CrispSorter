import hashlib
import importlib.util
from pathlib import Path
import plistlib
import shutil
import struct
import subprocess
import sys
import tempfile
import unittest
import zipfile

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location('mobile_build_receipt', ROOT / 'scripts/ocr_mobile_build_receipt.py')
receipt = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(receipt)


def elf(text=b'\x90\x90\xc3\0', debug=b'debug', machine=62):
    """Small structural ELF fixture; never executed or treated as native code."""
    names = b'\0.shstrtab\0.text\0.debug\0'
    count = 4 if debug else 3
    data = bytearray(192 + count * 64)
    struct.pack_into('<16sHHIQQQIHHHHHH', data, 0,
                     b'\x7fELF\x02\x01\x01' + b'\0' * 9, 3, machine, 1, 0, 0, 192, 0, 64, 0, 0, 64, count, 1)
    data[64:64 + len(names)] = names
    data[128:128 + len(text)] = text
    data[144:144 + len(debug)] = debug
    struct.pack_into('<IIQQQQIIQQ', data, 192 + 64, 1, 3, 0, 0, 64, len(names), 0, 0, 1, 0)
    struct.pack_into('<IIQQQQIIQQ', data, 192 + 128, 11, 1, 6, 4096, 128, len(text), 0, 0, 16, 0)
    if debug:
        struct.pack_into('<IIQQQQIIQQ', data, 192 + 192, 17, 1, 0, 0, 144, len(debug), 0, 0, 1, 0)
    return bytes(data)


class BuildReceiptTests(unittest.TestCase):
    @unittest.skipUnless(sys.platform.startswith('linux') and shutil.which('cc') and shutil.which('strip'), 'ELF strip control needs a Linux host C compiler and strip tool')
    def test_real_shared_library_keeps_identity_after_debug_stripping(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'identity.c').write_text('int identity_control(void) { return 42; }\n')
            library = root / 'identity.so'
            subprocess.run(['cc', '-shared', '-fPIC', '-g', str(root / 'identity.c'), '-o', str(library)], check=True, timeout=30, capture_output=True)
            before = library.read_bytes()
            subprocess.run(['strip', '--strip-unneeded', str(library)], check=True, timeout=30, capture_output=True)
            after = library.read_bytes()
            self.assertNotEqual(before, after)
            self.assertEqual(receipt.elf_allocated_identity(before), receipt.elf_allocated_identity(after))

    def test_stripped_debug_preserves_allocated_identity(self):
        self.assertNotEqual(elf(), elf(debug=b''))
        self.assertEqual(receipt.elf_allocated_identity(elf()), receipt.elf_allocated_identity(elf(debug=b'')))

    def test_changed_native_code_changes_identity(self):
        self.assertNotEqual(receipt.elf_allocated_identity(elf()), receipt.elf_allocated_identity(elf(text=b'\x90\x90\xc2\0')))

    def test_bad_or_truncated_runtime_is_rejected(self):
        for data in (b'not ELF', elf()[:80], elf(machine=40)):
            with self.subTest(size=len(data)), self.assertRaises(ValueError):
                receipt.elf_allocated_identity(data)

    def make_android(self, root, packaged=None, abi='x86_64'):
        native = elf()
        (root / 'lib').mkdir()
        (root / 'lib/libcrispembed.so').write_bytes(native)
        runtime = {'target': 'android-x86_64', 'artifacts': {'libcrispembed.so': hashlib.sha256(native).hexdigest()}}
        artifact = root / 'app.apk'
        with zipfile.ZipFile(artifact, 'w') as archive:
            archive.writestr(f'lib/{abi}/libcrispembed.so', elf(debug=b'') if packaged is None else packaged)
            archive.writestr(f'lib/{abi}/libcrispsorter.so', b'fake application library; identity fixture only')
        return runtime, artifact

    def test_apk_binds_actual_native_code_to_runtime_receipt(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            runtime, artifact = self.make_android(root)
            value = receipt.android_identity(artifact, root, runtime)
            self.assertTrue(value['paired_runtime_mapped_sections_match'])
            self.assertEqual(value['sha256'], receipt.digest(artifact))
            self.assertEqual(value['abis'], ['x86_64'])

    def test_apk_with_old_runtime_or_wrong_abi_fails(self):
        for packaged, abi in ((elf(text=b'old!'), 'x86_64'), (None, 'arm64-v8a')):
            with self.subTest(abi=abi), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                runtime, artifact = self.make_android(root, packaged, abi)
                with self.assertRaises(ValueError):
                    receipt.android_identity(artifact, root, runtime)

    def test_native_source_artifact_changed_after_build_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            runtime, artifact = self.make_android(root)
            (root / 'lib/libcrispembed.so').write_bytes(elf(text=b'evil'))
            with self.assertRaisesRegex(ValueError, 'Built runtime bytes'):
                receipt.android_identity(artifact, root, runtime)

    def test_ios_bundle_identity_detects_binary_changes_and_platform(self):
        with tempfile.TemporaryDirectory() as directory:
            app = Path(directory) / 'CrispSorter.app'
            app.mkdir()
            (app / 'Info.plist').write_bytes(plistlib.dumps({'CFBundleIdentifier': receipt.PACKAGE, 'DTPlatformName': 'iphonesimulator'}))
            executable = app / 'CrispSorter'
            executable.write_bytes(b'first')
            first = receipt.bundle_identity(app, 'iphonesimulator')
            executable.write_bytes(b'second')
            self.assertNotEqual(first['sha256'], receipt.bundle_identity(app, 'iphonesimulator')['sha256'])
            with self.assertRaisesRegex(ValueError, 'Apple platform'):
                receipt.bundle_identity(app, 'iphoneos')

    def test_ios_bundle_symlink_is_not_followed(self):
        with tempfile.TemporaryDirectory() as directory:
            app = Path(directory) / 'CrispSorter.app'
            app.mkdir()
            (app / 'Info.plist').write_bytes(plistlib.dumps({'CFBundleIdentifier': receipt.PACKAGE, 'DTPlatformName': 'iphoneos'}))
            (app / 'unsafe').symlink_to(Path(directory) / 'outside')
            with self.assertRaisesRegex(ValueError, 'symlinks'):
                receipt.bundle_identity(app, 'iphoneos')


if __name__ == '__main__':
    unittest.main()
