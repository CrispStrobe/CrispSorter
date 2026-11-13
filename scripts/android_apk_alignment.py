"""Inspect APK local data offsets without rewriting signed package bytes.

Stored native libraries are mapped directly from APKs and require 16 KiB ZIP
alignment as well as ELF load alignment. Compressed libraries are extracted;
their compressed ZIP offsets have no native page-alignment requirement.
"""
import struct
import zipfile


def check_alignment(path, *, require_four_byte=False):
    native = []
    with path.open('rb') as stream, zipfile.ZipFile(path) as archive:
        for entry in archive.infolist():
            if entry.compress_type != zipfile.ZIP_STORED or entry.is_dir():
                continue
            is_native = entry.filename.endswith('.so')
            if not is_native and not require_four_byte:
                continue
            stream.seek(entry.header_offset)
            header = stream.read(30)
            if len(header) != 30 or header[:4] != b'PK\x03\x04':
                raise ValueError('APK has an invalid local ZIP header: ' + entry.filename)
            name, extra = struct.unpack_from('<HH', header, 26)
            offset = entry.header_offset + 30 + name + extra
            if offset + entry.compress_size > archive.start_dir:
                raise ValueError('APK local ZIP data exceeds its archive: ' + entry.filename)
            if require_four_byte and offset % 4:
                raise ValueError('Run zipalign before signing: ' + entry.filename)
            if is_native:
                if offset % 16384:
                    raise ValueError('APK stored native library requires 16 KiB ZIP alignment: ' + entry.filename)
                native.append({'name': entry.filename, 'offset': offset,
                               'zip_page_alignment_16k': True})
    return native
