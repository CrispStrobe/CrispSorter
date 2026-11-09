import hashlib
import importlib.util
import io
import json
from pathlib import Path
import plistlib
import shutil
import struct
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
import zipfile

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'scripts'))
SPEC = importlib.util.spec_from_file_location('mobile_build_receipt', ROOT / 'scripts/ocr_mobile_build_receipt.py')
receipt = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(receipt)


def elf(text=b'\x90\x90\xc3\0', debug=b'debug', machine=62, alignment=16384):
    """Small structural ELF fixture; never executed or treated as native code."""
    names = b'\0.shstrtab\0.text\0.debug\0'
    count = 4 if debug else 3
    program_offset = 192 + count * 64
    data = bytearray(program_offset + 56)
    struct.pack_into('<16sHHIQQQIHHHHHH', data, 0,
                     b'\x7fELF\x02\x01\x01' + b'\0' * 9, 3, machine, 1, 0, program_offset, 192, 0, 64, 56, 1, 64, count, 1)
    struct.pack_into('<IIQQQQQQ', data, program_offset, 1, 5, 0, 0, 0, 160, 160, alignment)
    data[64:64 + len(names)] = names
    data[128:128 + len(text)] = text
    data[144:144 + len(debug)] = debug
    struct.pack_into('<IIQQQQIIQQ', data, 192 + 64, 1, 3, 0, 0, 64, len(names), 0, 0, 1, 0)
    struct.pack_into('<IIQQQQIIQQ', data, 192 + 128, 11, 1, 6, 4096, 128, len(text), 0, 0, 16, 0)
    if debug:
        struct.pack_into('<IIQQQQIIQQ', data, 192 + 192, 17, 1, 0, 0, 144, len(debug), 0, 0, 1, 0)
    return bytes(data)


class BuildReceiptTests(unittest.TestCase):
    def test_zip_library_inspection_streams_payload_and_reads_only_bounded_metadata(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            _, artifact = self.make_android(root)
            with zipfile.ZipFile(artifact) as archive:
                # A whole-member read would regress the failure seen with the
                # real 302.81 MiB debug application library.
                with patch.object(archive, 'read', side_effect=AssertionError('eager ZIP member read')):
                    inspected = receipt.inspect_apk_library(archive, 'lib/x86_64/libcrispsorter.so', 'x86_64')
            self.assertEqual(inspected['sha256'], hashlib.sha256(elf(text=b'app!')).hexdigest())
            self.assertEqual(inspected['bytes'], len(elf(text=b'app!')))
            self.assertEqual(inspected['elf']['minimum_load_alignment'], 16384)

    def test_file_section_hashing_never_requests_a_whole_large_section(self):
        class BoundedReader:
            def __init__(self, stream):
                self.stream, self.largest = stream, 0
            def fileno(self):
                return self.stream.fileno()
            def seek(self, *args):
                return self.stream.seek(*args)
            def read(self, count):
                self.largest = max(self.largest, count)
                if not 0 <= count <= receipt.INSPECTION_CHUNK_BYTES:
                    raise AssertionError('large eager ELF read')
                return self.stream.read(count)
        # A sparse, never-executed structural fixture gives its .text section
        # a large extent without storing an oversized repository artifact.
        data = bytearray(elf())
        section_bytes = 10 * receipt.INSPECTION_CHUNK_BYTES
        struct.pack_into('<Q', data, 192 + 128 + 32, section_bytes)
        with tempfile.TemporaryFile() as stream:
            stream.write(data)
            stream.truncate(128 + section_bytes)
            stream.flush()
            reader = BoundedReader(stream)
            identity = receipt.elf_allocated_identity(reader)
            self.assertEqual(identity['sections'][0]['size'], section_bytes)
            self.assertEqual(reader.largest, receipt.INSPECTION_CHUNK_BYTES)
            with self.assertRaisesRegex(ValueError, 'metadata exceeds'):
                receipt.ElfReader(reader).read(0, receipt.MAX_ELF_METADATA_BYTES + 1)

    def test_zip_actual_byte_count_and_member_budget_are_enforced(self):
        class Member:
            filename = 'lib/x86_64/fixture.so'
            def __init__(self, size): self.file_size = size
        class Archive:
            def __init__(self, payload, size): self.payload, self.member = payload, Member(size)
            def getinfo(self, name): return self.member
            def open(self, entry): return io.BytesIO(self.payload)
        data = elf()
        for declared in (len(data) - 1, len(data) + 1):
            with self.subTest(declared=declared), self.assertRaisesRegex(ValueError, 'actual bytes'):
                receipt.inspect_apk_library(Archive(data, declared), 'fixture', 'x86_64')
        with self.assertRaisesRegex(ValueError, 'member inspection budget'):
            receipt.inspect_apk_library(Archive(data, len(data)), 'fixture', 'x86_64', limit=len(data) - 1)
        result = receipt.inspect_apk_library(Archive(data, len(data)), 'fixture', 'x86_64', limit=len(data))
        self.assertEqual(result['bytes'], len(data))

    def test_aggregate_zip_and_metadata_budgets_reject_before_payload_inspection(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            _, artifact = self.make_android(root)
            with zipfile.ZipFile(artifact) as archive:
                total = sum(item.file_size for item in archive.infolist())
                with patch.object(receipt, 'MAX_APK_UNCOMPRESSED_BYTES', total - 1):
                    with self.assertRaisesRegex(ValueError, 'aggregate uncompressed'):
                        receipt.apk_inventory(archive)
                with patch.object(receipt, 'MAX_APK_MEMBERS', 1):
                    with self.assertRaisesRegex(ValueError, 'count bound'):
                        receipt.apk_inventory(archive)

    def test_overlapping_allocated_file_sections_are_rejected(self):
        data = bytearray(elf())
        struct.pack_into('<IIQQQQIIQQ', data, 192 + 192, 17, 1, 2, 4096, 128, 4, 0, 0, 1, 0)
        with self.assertRaisesRegex(ValueError, 'sections overlap'):
            receipt.elf_allocated_identity(data)

    def source_fixture(self, root):
        source = root / 'source'
        (source / 'static').mkdir(parents=True)
        (source / 'Cargo.lock').write_text('# immutable dependency lock fixture\nversion = 4\n')
        (source / 'app.rs').write_text('// immutable application source fixture\n')
        inventory = {'generatedAt': '2026-11-09T00:00:00Z',
                     'counts': {'frontend': 1, 'backend': 0, 'total': 1},
                     'licenses': [{'name': 'fixture', 'source': 'Frontend'}]}
        (source / 'static/licenses.json').write_text(json.dumps(inventory))
        for args in (['init', '-q'], ['add', '.'],
                     ['-c', 'user.name=Receipt control', '-c', 'user.email=receipt-control@example.invalid', 'commit', '-qm', 'fixture']):
            subprocess.run(['git', '-C', str(source), *args], check=True, capture_output=True, timeout=30)
        return source, inventory

    def test_source_receipt_records_generated_inventory_without_claiming_a_clean_tree(self):
        with tempfile.TemporaryDirectory() as directory:
            source, inventory = self.source_fixture(Path(directory))
            clean = receipt.source_tree_identity(source)
            self.assertTrue(clean['tracked_source_clean'])
            self.assertEqual(clean['generated_tracked_files'], [])
            inventory['generatedAt'] = '2026-11-09T01:00:00Z'
            (source / 'static/licenses.json').write_text(json.dumps(inventory))
            generated = receipt.source_tree_identity(source)
            self.assertTrue(generated['application_source_clean'])
            self.assertFalse(generated['tracked_source_clean'])
            self.assertEqual(generated['cargo_lock_sha256'], receipt.digest(source / 'Cargo.lock'))
            entry = generated['generated_tracked_files'][0]
            self.assertEqual(entry['path'], 'static/licenses.json')
            self.assertEqual(entry['sha256'], receipt.digest(source / entry['path']))
            self.assertNotEqual(entry['sha256'], entry['committed_sha256'])

    def test_source_receipt_accepts_explicit_native_notices_without_losing_count_validation(self):
        with tempfile.TemporaryDirectory() as directory:
            source, inventory = self.source_fixture(Path(directory))
            inventory['licenses'].append({'name': 'SheenBidi fixture', 'source': 'Native'})
            inventory['counts'].update(native=1, total=2)
            (source / 'static/licenses.json').write_text(json.dumps(inventory))
            value = receipt.source_tree_identity(source)
            self.assertEqual(value['generated_tracked_files'][0]['counts']['native'], 1)
            inventory['counts']['native'] = 0
            (source / 'static/licenses.json').write_text(json.dumps(inventory))
            with self.assertRaisesRegex(ValueError, 'counts/content'):
                receipt.source_tree_identity(source)
            del inventory['counts']['native']
            (source / 'static/licenses.json').write_text(json.dumps(inventory))
            with self.assertRaisesRegex(ValueError, 'counts/content'):
                receipt.source_tree_identity(source)

    def test_source_receipt_still_rejects_changed_code_lock_and_untracked_files(self):
        for name in ('app.rs', 'Cargo.lock', 'untracked.rs'):
            with self.subTest(name=name), tempfile.TemporaryDirectory() as directory:
                source, inventory = self.source_fixture(Path(directory))
                inventory['generatedAt'] = '2026-11-09T01:00:00Z'
                (source / 'static/licenses.json').write_text(json.dumps(inventory))
                (source / name).write_text('changed fixture')
                with self.assertRaisesRegex(ValueError, 'Application source or dependency lock'):
                    receipt.source_tree_identity(source)

    def test_source_receipt_rejects_malformed_inventories_and_symlinked_generated_data(self):
        with tempfile.TemporaryDirectory() as directory:
            source, inventory = self.source_fixture(Path(directory))
            target = source / 'static/licenses.json'
            inventory['counts']['total'] = 2
            target.write_text(json.dumps(inventory))
            with self.assertRaisesRegex(ValueError, 'inconsistent'):
                receipt.source_tree_identity(source)
            target.write_text('{}')
            with self.assertRaisesRegex(ValueError, 'unknown schema'):
                receipt.source_tree_identity(source)
            outside = Path(directory) / 'outside.json'
            outside.write_text('never adopt external data')
            target.unlink()
            target.symlink_to(outside)
            with self.assertRaisesRegex(ValueError, 'regular source-contained'):
                receipt.source_tree_identity(source)

    @unittest.skipUnless(sys.platform.startswith('linux') and shutil.which('cc') and shutil.which('strip'), 'ELF strip control needs a Linux host C compiler and strip tool')
    def test_real_shared_library_keeps_identity_after_debug_stripping(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'identity.c').write_text('int identity_control(void) { return 42; }\n')
            library = root / 'identity.so'
            subprocess.run(['cc', '-shared', '-fPIC', '-g', '-Wl,-z,max-page-size=16384', str(root / 'identity.c'), '-o', str(library)], check=True, timeout=30, capture_output=True)
            before = library.read_bytes()
            subprocess.run(['strip', '--strip-unneeded', str(library)], check=True, timeout=30, capture_output=True)
            after = library.read_bytes()
            self.assertNotEqual(before, after)
            self.assertEqual(receipt.elf_allocated_identity(before), receipt.elf_allocated_identity(after))
            self.assertEqual(receipt.android_load_identity(before, 'x86_64'), receipt.android_load_identity(after, 'x86_64'))

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
            archive.writestr(f'lib/{abi}/libcrispsorter.so', elf(text=b'app!'))
        return runtime, artifact

    def test_apk_binds_actual_native_code_to_runtime_receipt(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            runtime, artifact = self.make_android(root)
            value = receipt.android_identity(artifact, root, runtime)
            self.assertTrue(value['paired_runtime_mapped_sections_match'])
            self.assertEqual(value['sha256'], receipt.digest(artifact))
            self.assertEqual(value['abis'], ['x86_64'])
            self.assertEqual(value['native_elf']['lib/x86_64/libcrispsorter.so']['minimum_load_alignment'], 16384)

    def test_final_apk_application_library_must_support_16_kib_loads(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            runtime, artifact = self.make_android(root)
            with zipfile.ZipFile(artifact, 'w') as archive:
                archive.writestr('lib/x86_64/libcrispembed.so', elf(debug=b''))
                archive.writestr('lib/x86_64/libcrispsorter.so', elf(text=b'app!', alignment=4096))
            with self.assertRaisesRegex(ValueError, 'non-16-KiB'):
                receipt.android_identity(artifact, root, runtime)

    def test_packaged_elf_rejects_wrong_abi_bad_headers_and_incongruent_loads(self):
        bad_address = bytearray(elf())
        offset = struct.unpack_from('<Q', bad_address, 32)[0]
        struct.pack_into('<Q', bad_address, offset + 16, 1)
        for data in (elf(machine=183), elf()[:-1], bytes(bad_address), elf(alignment=24576)):
            with self.subTest(size=len(data)), self.assertRaises(ValueError):
                receipt.android_load_identity(data, 'x86_64')

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

    def make_ort(self, root, packaged=None, include_notice=True):
        source, runtime = root / 'source', root / 'ort'
        (source / 'scripts').mkdir(parents=True)
        (source / 'src-tauri').mkdir()
        (source / '.cargo').mkdir()
        (runtime / 'lib').mkdir(parents=True)
        (runtime / 'include').mkdir()
        native, header = elf(), b'#define ORT_API_VERSION 23\n'
        (runtime / 'lib/libonnxruntime.so').write_bytes(native)
        (runtime / 'include/onnxruntime_c_api.h').write_bytes(header)
        notices = [dict(filename=name, bytes=len(data), sha256=hashlib.sha256(data).hexdigest())
                   for name, data in [('LICENSE', b'license fixture'), ('ThirdPartyNotices.txt', b'notice fixture')]]
        pin = dict(version='1.23.2', url='https://invalid/fixture.aar', filename='fixture.aar', bytes=1, sha256='0' * 64,
                   libraries={'x86_64': {'sha256': hashlib.sha256(native).hexdigest()}})
        lock = dict(rust_binding='2.0.0-rc.11', api_version=23, android=pin, header_sha256=hashlib.sha256(header).hexdigest(), notices=notices)
        lock_path = source / 'scripts/ocr-mobile-ort-lock.json'
        lock_path.write_text(json.dumps(lock))
        (source / 'src-tauri/Cargo.toml').write_text('[dependencies]\nort = "=2.0.0-rc.11"\n')
        (source / '.cargo/config.toml').write_text('[env]\nORT_LIB_LOCATION = { value = ' + json.dumps(str(runtime / 'lib')) + ', force = true }\nORT_PREFER_DYNAMIC_LINK = { value = "1", force = true }\n')
        value = dict(kind='ocr-mobile-ort-runtime', target='android-x86_64', version=pin['version'], api_version=23,
                     rust_binding=lock['rust_binding'], lock_sha256=receipt.digest(lock_path),
                     archive={key: pin[key] for key in ('url', 'filename', 'bytes', 'sha256')},
                     files={name: {'bytes': len(data), 'sha256': hashlib.sha256(data).hexdigest()} for name, data
                            in [('lib/libonnxruntime.so', native), ('include/onnxruntime_c_api.h', header)]})
        runtime_path = runtime / 'ort-runtime.json'
        runtime_path.write_text(json.dumps(value))
        artifact = root / 'app.apk'
        with zipfile.ZipFile(artifact, 'w') as archive:
            archive.writestr('lib/x86_64/libonnxruntime.so', packaged if packaged is not None else elf(debug=b''))
            if include_notice:
                archive.writestr('assets/bin/onnxruntime-LICENSE.txt', b'license fixture')
            archive.writestr('assets/bin/onnxruntime-ThirdPartyNotices.txt', b'notice fixture')
        return source, artifact, runtime_path

    def test_ort_identity_binds_stripped_packaged_code_and_notices(self):
        with tempfile.TemporaryDirectory() as directory:
            args = self.make_ort(Path(directory))
            value = receipt.ort_identity(args[0], 'android', args[1], args[2], 'android-x86_64')
            self.assertTrue(value['linkage']['mapped_sections_match'])
            self.assertEqual(len(value['packaged_notices']), 2)

    def test_old_packaged_ort_and_missing_license_cannot_pass_identity(self):
        for settings in ({'packaged': elf(text=b'old!')}, {'include_notice': False}):
            with self.subTest(settings=settings), tempfile.TemporaryDirectory() as directory:
                args = self.make_ort(Path(directory), **settings)
                with self.assertRaises(ValueError):
                    receipt.ort_identity(args[0], 'android', args[1], args[2], 'android-x86_64')

    def test_ort_binding_and_nested_cargo_path_must_match_receipt(self):
        for filename, replacement in [('src-tauri/Cargo.toml', '[dependencies]\nort = "=2.0.0-rc.12"\n'), ('.cargo/config.toml', '[env]\n')]:
            with self.subTest(filename=filename), tempfile.TemporaryDirectory() as directory:
                args = self.make_ort(Path(directory))
                (args[0] / filename).write_text(replacement)
                with self.assertRaises(ValueError):
                    receipt.ort_identity(args[0], 'android', args[1], args[2], 'android-x86_64')

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
