#!/usr/bin/env python3
"""Verify one exact defined ARM64 Mach-O code symbol without dumping all names.

Only the bounded LC_SYMTAB/string-table index is searched. String presence,
undefined imports, debug records and names pointing outside an instruction
section cannot establish linkage. No executable code is run or modified.
"""
import argparse
import hashlib
import json
import mmap
from pathlib import Path
import struct
import time

MAX_FILE_BYTES = 2 * 1024**3
MAX_LOAD_COMMAND_BYTES = 16 * 1024**2
MAX_LOAD_COMMANDS = 8192
MAX_SYMBOLS = 8_000_000
MAX_STRING_BYTES = 256 * 1024**2
MAX_NAME_MATCHES = 4096
MAX_SECONDS = 30
SYMBOL = '_OrtGetApiBase'


def defined_code_symbol(path, symbol=SYMBOL):
    path = Path(path)
    if (path.is_symlink() or not path.is_file()
            or not 32 <= path.stat().st_size <= MAX_FILE_BYTES):
        raise ValueError('Mach-O symbol proof requires a bounded regular executable')
    if not isinstance(symbol, str) or not symbol.isascii() or not 1 <= len(symbol) <= 256 or '\0' in symbol:
        raise ValueError('Mach-O symbol name must be bounded ASCII without NUL')
    deadline = time.monotonic() + MAX_SECONDS

    def budget():
        if time.monotonic() > deadline:
            raise ValueError('Mach-O symbol proof exceeded its bounded inspection time')

    with path.open('rb') as stream, mmap.mmap(stream.fileno(), 0, access=mmap.ACCESS_READ) as data:
        size = len(data)

        def bounds(offset, length, label):
            if offset < 0 or length < 0 or offset > size or length > size - offset:
                raise ValueError('Mach-O ' + label + ' exceeds executable bounds')

        magic, cpu, subtype, filetype, count, commands_size, flags, reserved = struct.unpack_from('<IiiIIIII', data)
        if magic != 0xfeedfacf or cpu != 0x0100000c or filetype != 2:
            raise ValueError('Expected a thin little-endian ARM64 Mach-O executable')
        if count > MAX_LOAD_COMMANDS or commands_size > MAX_LOAD_COMMAND_BYTES:
            raise ValueError('Mach-O load commands exceed the inspection budget')
        bounds(32, commands_size, 'load commands')
        command_end = 32 + commands_size
        at, table, sections = 32, None, []
        for _ in range(count):
            budget()
            if at + 8 > command_end:
                raise ValueError('Truncated Mach-O load command')
            command, length = struct.unpack_from('<II', data, at)
            if length < 8 or length % 8 or length > command_end - at:
                raise ValueError('Invalid Mach-O load command length')
            if command == 2:  # LC_SYMTAB
                if table is not None or length != 24:
                    raise ValueError('Mach-O requires one unambiguous LC_SYMTAB')
                table = struct.unpack_from('<IIII', data, at + 8)
            elif command == 0x19:  # LC_SEGMENT_64; section indices are global and 1-based.
                if length < 72:
                    raise ValueError('Truncated Mach-O segment')
                parent_name = data[at + 8:at + 24].split(b'\0')[0].decode('ascii')
                vmaddr, vmsize, segment_file, segment_bytes = struct.unpack_from('<QQQQ', data, at + 24)
                initial_protection = struct.unpack_from('<I', data, at + 60)[0]
                bounds(segment_file, segment_bytes, 'segment file range')
                if vmaddr + vmsize > 2**64:
                    raise ValueError('Mach-O segment address overflows')
                nsections = struct.unpack_from('<I', data, at + 64)[0]
                if len(sections) + nsections > 255 or 72 + 80 * nsections > length:
                    raise ValueError('Mach-O sections exceed command bounds')
                for index in range(nsections):
                    offset = at + 72 + index * 80
                    name = data[offset:offset + 16].split(b'\0')[0].decode('ascii')
                    segment = data[offset + 16:offset + 32].split(b'\0')[0].decode('ascii')
                    address, nbytes, file_offset = struct.unpack_from('<QQI', data, offset + 32)
                    section_flags = struct.unpack_from('<I', data, offset + 64)[0]
                    if segment != parent_name or address < vmaddr or address + nbytes > vmaddr + vmsize:
                        raise ValueError('Mach-O section exceeds its parent segment')
                    sections.append((segment, name, address, nbytes, file_offset, section_flags,
                                     segment_file, segment_bytes, initial_protection))
            at += length
        if at != command_end or table is None:
            raise ValueError('Mach-O load command table is incomplete')
        symbol_offset, nsyms, string_offset, string_bytes = table
        if nsyms > MAX_SYMBOLS or string_bytes > MAX_STRING_BYTES:
            raise ValueError('Mach-O symbol/string tables exceed the inspection budget')
        bounds(symbol_offset, 16 * nsyms, 'symbol table')
        bounds(string_offset, string_bytes, 'string table')
        if (symbol_offset < command_end or string_offset < symbol_offset + 16 * nsyms):
            raise ValueError('Mach-O symbol/string tables overlap executable metadata')
        needle, candidates, position = symbol.encode('ascii') + b'\0', set(), string_offset
        while True:
            budget()
            position = data.find(needle, position, string_offset + string_bytes)
            if position < 0:
                break
            # Suffix-sharing string tables are legal: n_strx, not the byte
            # before a candidate name, determines the exact symbol boundary.
            candidates.add(position - string_offset)
            if len(candidates) > MAX_NAME_MATCHES:
                raise ValueError('Mach-O symbol name matches exceed the inspection budget')
            position += len(needle)
        if not candidates:
            raise ValueError('Built iOS app does not define exactly one ONNX Runtime C API symbol')
        matches = []
        for index in range(nsyms):
            if index % 65536 == 0:
                budget()
            entry = symbol_offset + 16 * index
            string_index = struct.unpack_from('<I', data, entry)[0]
            if string_index not in candidates:
                continue
            _, kind, section_index, description, address = struct.unpack_from('<IBBHQ', data, entry)
            # N_STAB records are debug information; N_UNDF/ABS/INDR do not
            # demonstrate a real function definition in this executable.
            if (kind & 0xe0) or (kind & 0x0e) != 0x0e:
                continue
            if not 1 <= section_index <= len(sections):
                raise ValueError('Defined Mach-O symbol has an invalid section index')
            segment, name, start, length, file_offset, section_flags, segment_file, segment_bytes, protection = sections[section_index - 1]
            if (not (section_flags & (0x80000000 | 0x400)) or not start <= address < start + length
                    or address % 4 or segment not in ('__TEXT', '__TEXT_EXEC') or not (protection & 4)
                    or (section_flags & 0xff) in (1, 0xc, 0x12)
                    or file_offset < segment_file or file_offset + length > segment_file + segment_bytes):
                raise ValueError('Mach-O symbol does not address a defined instruction section')
            bounds(file_offset, length, 'instruction section')
            if matches:
                raise ValueError('Built iOS app has ambiguous duplicate ONNX Runtime code symbol definitions')
            matches.append({'symbol_index': index, 'type': hex(kind), 'section': segment + ',' + name,
                            'address': hex(address)})
        if len(matches) != 1:
            raise ValueError('Built iOS app does not define exactly one ONNX Runtime C API symbol')
        stream.seek(0)
        digest = hashlib.sha256()
        for chunk in iter(lambda: stream.read(4 * 1024**2), b''):
            budget()
            digest.update(chunk)
        return {'schema': 'crispsorter-macho-defined-code-symbol-v1', 'symbol': symbol, 'defined': True,
                **matches[0], 'executable_sha256': digest.hexdigest(), 'executable_bytes': size,
                'symbol_table_entries': nsyms, 'string_table_bytes': string_bytes,
                'verification': 'Exact nlist_64 name/definition and instruction-section address; no nm dump or string-only fallback'}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('executable', type=Path)
    args = parser.parse_args()
    print(json.dumps(defined_code_symbol(args.executable), indent=2))
