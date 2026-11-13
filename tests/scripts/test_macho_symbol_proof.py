"""Structural final-app C API proof rejects imports and string-only lookalikes."""
import importlib.util
from pathlib import Path
import struct
import tempfile
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).resolve().parents[2] / 'scripts/macho_symbol_proof.py'
SPEC = importlib.util.spec_from_file_location('macho_symbol_proof', SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)
BASE = 0x100000000


def macho(symbols=None, strings=b'\0_OrtGetApiBase\0', text=b'\xc0\x03\x5f\xd6', flags=0x80000400, protection=5):
    symbols = symbols if symbols is not None else [(1, 0x0f, 1, BASE + 208)]
    header = struct.pack('<IiiIIIII', 0xfeedfacf, 0x0100000c, 0, 2, 2, 176, 0, 0)
    segment = struct.pack('<II16sQQQQiiII', 0x19, 152, b'__TEXT', BASE, 0x1000, 0, 208 + len(text), 7, protection, 1, 0)
    section = struct.pack('<16s16sQQIIIIIIII', b'__text', b'__TEXT', BASE + 208, len(text), 208, 2, 0, 0, flags, 0, 0, 0)
    symbol_offset = 208 + len(text)
    table = b''.join(struct.pack('<IBBHQ', string, kind, index, 0, address) for string, kind, index, address in symbols)
    command = struct.pack('<6I', 2, 24, symbol_offset, len(symbols), symbol_offset + len(table), len(strings))
    return bytearray(header + segment + section + command + text + table + strings)


class SymbolProof(unittest.TestCase):
    def check(self, data):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'executable'
            path.write_bytes(data)
            return MODULE.defined_code_symbol(path)

    def test_exact_defined_instruction_symbol_yields_bounded_hash_bound_proof(self):
        result = self.check(macho())
        self.assertTrue(result['defined'])
        self.assertEqual(result['symbol'], '_OrtGetApiBase')
        self.assertEqual(result['section'], '__TEXT,__text')
        self.assertEqual(result['address'], hex(BASE + 208))
        self.assertEqual(len(result['executable_sha256']), 64)
        self.assertEqual(result['symbol_table_entries'], 1)

    def test_local_defined_symbol_is_accepted_without_requiring_dynamic_export(self):
        result = self.check(macho([(1, 0x0e, 1, BASE + 208)]))
        self.assertEqual(result['type'], '0xe')

    def test_undefined_debug_absolute_or_indirect_names_never_establish_linkage(self):
        for kind in [0x01, 0x24, 0x03, 0x0b]:
            with self.subTest(kind=kind), self.assertRaisesRegex(ValueError, 'does not define exactly one'):
                self.check(macho([(1, kind, 1, BASE + 208)]))

    def test_string_presence_or_longer_symbol_suffix_does_not_establish_definition(self):
        for data in [macho([]), macho([(1, 0x0f, 1, BASE + 208)], strings=b'\0wrong_OrtGetApiBase\0'),
                     macho(strings=b'\0_OrtGetApiBase_extra\0')]:
            with self.assertRaisesRegex(ValueError, 'does not define exactly one'):
                self.check(data)

    def test_suffix_shared_string_index_is_resolved_by_exact_n_strx(self):
        strings = b'\0prefix_OrtGetApiBase\0'
        result = self.check(macho([(7, 0x0f, 1, BASE + 208)], strings=strings))
        self.assertTrue(result['defined'])

    def test_symbol_requires_valid_section_executable_mapping_and_aligned_address(self):
        cases = [macho([(1, 0x0f, 0, BASE + 208)]), macho([(1, 0x0f, 2, BASE + 208)]),
                 macho([(1, 0x0f, 1, BASE + 212)]), macho([(1, 0x0f, 1, BASE + 209)]),
                 macho(flags=0), macho(flags=0x80000401), macho(protection=3)]
        for data in cases:
            with self.assertRaises(ValueError):
                self.check(data)

    def test_duplicate_definitions_are_ambiguous_and_rejected(self):
        with self.assertRaisesRegex(ValueError, 'ambiguous duplicate'):
            self.check(macho([(1, 0x0f, 1, BASE + 208)] * 2))

    def test_table_offsets_counts_and_load_command_shapes_are_bounded(self):
        original = macho()
        corruptions = [(16, MODULE.MAX_LOAD_COMMANDS + 1), (20, MODULE.MAX_LOAD_COMMAND_BYTES + 1),
                       (36, 0), (196, MODULE.MAX_SYMBOLS + 1), (200, len(original) + 100),
                       (204, MODULE.MAX_STRING_BYTES + 1), (192, 1)]
        for offset, value in corruptions:
            data = original.copy()
            struct.pack_into('<I', data, offset, value)
            with self.subTest(offset=offset), self.assertRaises(ValueError):
                self.check(data)
        for length in [0, 31, 40, 190, len(original) - 1]:
            with self.subTest(length=length), self.assertRaises(ValueError):
                self.check(original[:length])

    def test_duplicate_symbol_command_is_rejected(self):
        data = macho()
        duplicate = data[184:208]
        data[208:208] = duplicate
        struct.pack_into('<II', data, 16, 3, 200)
        with self.assertRaisesRegex(ValueError, 'unambiguous LC_SYMTAB'):
            self.check(data)

    def test_repeated_name_candidates_cannot_grow_an_unbounded_index(self):
        data = macho(strings=b'\0' + b'_OrtGetApiBase\0' * (MODULE.MAX_NAME_MATCHES + 1))
        with self.assertRaisesRegex(ValueError, 'name matches exceed'):
            self.check(data)

    def test_unexpected_architecture_filetype_and_parent_segment_are_rejected(self):
        for offset, value in [(0, 0xfeedface), (4, 0x01000007), (12, 1), (120, ord('X'))]:
            data = macho()
            struct.pack_into('<I', data, offset, value)
            with self.subTest(offset=offset), self.assertRaises(ValueError):
                self.check(data)

    def test_symlink_oversized_file_and_time_budget_fail_closed(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory);path = root / 'executable';path.write_bytes(macho())
            link = root / 'link';link.symlink_to(path)
            with self.assertRaises(ValueError):MODULE.defined_code_symbol(link)
            with path.open('r+b') as stream:stream.truncate(MODULE.MAX_FILE_BYTES + 1)
            with self.assertRaises(ValueError):MODULE.defined_code_symbol(path)
        with patch.object(MODULE, 'MAX_SECONDS', -1), self.assertRaisesRegex(ValueError, 'bounded inspection time'):
            self.check(macho())


if __name__ == '__main__':
    unittest.main()
