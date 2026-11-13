"""Real ZIP-offset fixtures; structural ELF payloads are never executed."""
from pathlib import Path
import struct
import sys
import tempfile
import unittest
from unittest.mock import patch
import zipfile

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'scripts'))
sys.path.insert(0, str(Path(__file__).resolve().parent))
import android_apk_alignment as alignment
import sign_android_apk as signing
import test_mobile_build_receipt as fixture

elf, receipt = fixture.elf, fixture.receipt


def write_member(archive, name, data, *, boundary=16384, compressed=False):
    entry = zipfile.ZipInfo(name)
    entry.compress_type = zipfile.ZIP_DEFLATED if compressed else zipfile.ZIP_STORED
    if not compressed:
        padding = -(archive.fp.tell() + 30 + len(name.encode('utf-8'))) % boundary
        if 0 < padding < 4:
            padding += boundary
        if padding:
            entry.extra = struct.pack('<HH', 0xcafe, padding - 4) + bytes(padding - 4)
    archive.writestr(entry, data)


class ApkAlignmentTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.apk = self.root / 'fixture.apk'

    def make_zip(self, *, boundary=16384, compressed=False, name='lib/arm64-v8a/libfixture.so'):
        with zipfile.ZipFile(self.apk, 'w') as archive:
            write_member(archive, name, b'fixture', boundary=boundary, compressed=compressed)

    def test_stored_native_libraries_require_actual_16k_local_data_offsets(self):
        self.make_zip()
        rows = signing.check_alignment(self.apk)
        self.assertEqual(rows, alignment.check_alignment(self.apk))
        self.assertEqual(rows[0]['offset'], 16384)
        self.assertTrue(rows[0]['zip_page_alignment_16k'])
        self.make_zip(boundary=4)
        for check in (signing.check_alignment, alignment.check_alignment):
            with self.subTest(check=check.__module__), self.assertRaisesRegex(ValueError, '16 KiB ZIP alignment'):
                check(self.apk)

    def test_unicode_native_filename_uses_encoded_header_length(self):
        self.make_zip(name='lib/arm64-v8a/lib\u00e9.so')
        self.assertEqual(signing.check_alignment(self.apk)[0]['offset'], 16384)

    def test_compressed_native_member_needs_no_zip_page_alignment(self):
        self.make_zip(compressed=True)
        self.assertEqual(signing.check_alignment(self.apk), [])

    def test_existing_non_native_four_byte_signing_gate_remains(self):
        with zipfile.ZipFile(self.apk, 'w') as archive:
            archive.writestr('a', b'fixture')  # data starts at byte 31
        self.assertEqual(alignment.check_alignment(self.apk), [])
        with self.assertRaisesRegex(ValueError, 'Run zipalign'):
            signing.check_alignment(self.apk)

    def test_malformed_local_header_and_out_of_archive_offset_fail_closed(self):
        for change in ('signature', 'extra-length'):
            self.make_zip()
            with self.apk.open('r+b') as stream:
                if change == 'signature':
                    stream.write(b'BAD!')
                else:
                    stream.seek(28)
                    stream.write(struct.pack('<H', 65535))
            with self.subTest(change=change), self.assertRaisesRegex(ValueError, 'local ZIP'):
                alignment.check_alignment(self.apk)

    def test_signer_rejects_before_invoking_signing_tools_or_writing_output(self):
        self.make_zip(boundary=4)
        output = self.root / 'signed.apk'
        with patch.object(sys, 'argv', ['sign_android_apk.py', str(self.apk), str(output)]), \
                patch.object(signing.subprocess, 'run') as run, \
                self.assertRaisesRegex(ValueError, '16 KiB ZIP alignment'):
            signing.main()
        run.assert_not_called()
        self.assertFalse(output.exists())

    def test_final_receipt_independently_rejects_zip_misalignment_before_elf_inspection(self):
        runtime, self.apk = fixture.BuildReceiptTests().make_android(self.root)
        with zipfile.ZipFile(self.apk, 'w') as archive:
            write_member(archive, 'lib/x86_64/libcrispembed.so', elf(debug=b''), boundary=4)
            write_member(archive, 'lib/x86_64/libcrispsorter.so', elf(text=b'app!'), boundary=4)
        with patch.object(receipt, 'inspect_apk_library') as inspect, \
                self.assertRaisesRegex(ValueError, '16 KiB ZIP alignment'):
            receipt.android_identity(self.apk, self.root, runtime)
        inspect.assert_not_called()

    def test_final_receipt_records_each_aligned_native_library(self):
        runtime, self.apk = fixture.BuildReceiptTests().make_android(self.root)
        with zipfile.ZipFile(self.apk, 'w') as archive:
            write_member(archive, 'lib/x86_64/libcrispembed.so', elf(debug=b''))
            write_member(archive, 'lib/x86_64/libcrispsorter.so', elf(text=b'app!'))
        result = receipt.android_identity(self.apk, self.root, runtime)
        self.assertEqual(len(result['native_zip_alignment']), 2)
        self.assertTrue(all(row['offset'] % 16384 == 0 for row in result['native_zip_alignment']))


if __name__ == '__main__':
    unittest.main()
