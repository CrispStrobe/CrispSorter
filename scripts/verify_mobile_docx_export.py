#!/usr/bin/env python3
"""Independently read one bounded native Word fixture; no Word app is invoked."""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import platform
import xml.etree.ElementTree as ET
import zipfile

MAX_ARCHIVE_BYTES = 16 * 1024 * 1024
MAX_UNCOMPRESSED_BYTES = 32 * 1024 * 1024
MAX_XML_BYTES = 8 * 1024 * 1024
W = '{http://schemas.openxmlformats.org/wordprocessingml/2006/main}'
CT = '{http://schemas.openxmlformats.org/package/2006/content-types}'
REL = '{http://schemas.openxmlformats.org/package/2006/relationships}'
XML_SPACE = '{http://www.w3.org/XML/1998/namespace}space'
WORD_TYPE = 'application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml'
OFFICE_REL = 'http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument'


def require(condition, message):
    if not condition:
        raise ValueError(message)


def xml_part(archive, name):
    info = archive.getinfo(name)
    require(0 < info.file_size <= MAX_XML_BYTES, 'OOXML part exceeds the reader bound: ' + name)
    data = archive.read(name)
    require(len(data) == info.file_size, 'OOXML part length differs from its ZIP metadata')
    text = data.decode('utf-8-sig')
    require('<!DOCTYPE' not in text and '<!ENTITY' not in text, 'Generated OOXML contains a DTD/entity declaration')
    return ET.fromstring(text), data


def property_enabled(paragraph, name):
    element = paragraph.find(W + 'pPr/' + W + name)
    if element is None:
        return False
    value = element.get(W + 'val', 'true').lower()
    require(value in {'true', 'false', '1', '0', 'on', 'off'}, 'Invalid OOXML on/off property: ' + name)
    return value in {'true', '1', 'on'}


def paragraph_text(paragraph):
    parts = []
    for item in paragraph.iter():
        if item.tag == W + 't':
            value = item.text or ''
            if value and (value[0].isspace() or value[-1].isspace()):
                require(item.get(XML_SPACE) == 'preserve', 'Word whitespace lacks xml:space preservation')
            parts.append(value)
        elif item.tag == W + 'tab':
            parts.append('\t')
        elif item.tag in {W + 'br', W + 'cr'}:
            parts.append('\n')
    return ''.join(parts)


def verify(path: Path, expected: dict) -> dict:
    require(expected.get('schema_version') == 1, 'Unknown expected-review schema')
    paragraphs = expected.get('paragraphs')
    require(isinstance(paragraphs, list) and 1 <= len(paragraphs) <= 100
            and all(isinstance(text, str) and len(text) <= 16_384 for text in paragraphs),
            'Expected reviewed paragraphs are missing or exceed the fixture bound')
    mixed = expected.get('mixed_rtl_paragraph')
    require(isinstance(mixed, str) and paragraphs.count(mixed) == 1, 'One explicit mixed-direction review paragraph is required')
    expected_text = '\n'.join(paragraphs)
    require('2049' in expected_text and '2048' not in expected_text, 'Expected review lacks the declared 2048 to 2049 correction')
    require(path.is_file() and not path.is_symlink() and 0 < path.stat().st_size <= MAX_ARCHIVE_BYTES,
            'Word artifact is missing, unsafe, or exceeds the bounded fixture size')
    with path.open('rb') as stream:
        sha256 = hashlib.file_digest(stream, 'sha256').hexdigest()
    with zipfile.ZipFile(path) as archive:
        members = archive.infolist()
        names = [item.filename for item in members]
        require(1 <= len(names) <= 256 and len(names) == len(set(names)), 'Duplicate or excessive Word package members')
        require(all(not PurePosixPath(name).is_absolute() and '..' not in PurePosixPath(name).parts for name in names),
                'Unexpected Word package member path')
        require(sum(item.file_size for item in members) <= MAX_UNCOMPRESSED_BYTES,
                'Word package exceeds the uncompressed reader bound')
        require(not any(item.flag_bits & 1 for item in members), 'Encrypted fixture packages are unsupported')
        require(archive.testzip() is None, 'Word package CRC verification failed')
        content_types, _ = xml_part(archive, '[Content_Types].xml')
        require(content_types.tag == CT + 'Types', 'Missing OPC content-type namespace')
        main_types = [item for item in content_types.findall(CT + 'Override') if item.get('PartName') == '/word/document.xml']
        require(len(main_types) == 1 and main_types[0].get('ContentType') == WORD_TYPE, 'Package is not a Word document')
        relations, _ = xml_part(archive, '_rels/.rels')
        require(relations.tag == REL + 'Relationships', 'Missing OPC relationship namespace')
        main = [item for item in relations.findall(REL + 'Relationship') if item.get('Type') == OFFICE_REL]
        require(len(main) == 1 and main[0].get('Target', '').lstrip('/') == 'word/document.xml'
                and main[0].get('TargetMode', 'Internal') == 'Internal', 'Word package has no unique internal main document')
        document, xml_bytes = xml_part(archive, 'word/document.xml')
        require(document.tag == W + 'document', 'Unexpected Word document namespace')
        body = document.find(W + 'body')
        require(body is not None, 'Word document has no body')
        nodes = body.findall(W + 'p')
        require(len(list(document.iter(W + 'p'))) == len(nodes), 'Text-only fixture contains unexpected nested paragraphs')
        actual = [paragraph_text(item) for item in nodes]
        require(actual == paragraphs, 'Word logical text/order differs from the persisted reviewed state')
        bidi = [property_enabled(item, 'bidi') for item in nodes]
        require(bidi[paragraphs.index(mixed)], 'Mixed Hebrew/Latin/Arabic review lost its RTL paragraph property')
        breaks = [property_enabled(item, 'pageBreakBefore') for item in nodes]
        require(not any(breaks), 'Single-source-page fixture gained an explicit page break')
    return {'schema_version': 1, 'kind': 'mobile-docx-readback', 'status': 'passed',
            'reader': 'Python zipfile and ElementTree', 'python_version': platform.python_version(),
            'archive_sha256': sha256, 'archive_bytes': path.stat().st_size,
            'document_xml_sha256': hashlib.sha256(xml_bytes).hexdigest(),
            'paragraphs': actual, 'paragraph_rtl': bidi, 'page_break_before': breaks,
            'correction_verified': '2048 -> 2049', 'zip_crc_verified': True,
            'word_application_tested': False, 'provider_copy_tested': False,
            'qualification': 'Package structure and exact logical reviewed text, tabs, spaces and paragraph direction only. No Word rendering, visual pagination or provider transfer qualification.'}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--artifact', type=Path, required=True)
    parser.add_argument('--expected', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    result = {'schema_version': 1, 'kind': 'mobile-docx-readback', 'status': 'failed'}
    try:
        require(args.expected.stat().st_size <= 256 * 1024, 'Expected reviewed-state receipt exceeds its bound')
        result = verify(args.artifact, json.loads(args.expected.read_text()))
    except Exception as error:
        result['error'] = f'{type(error).__name__}: {error}'
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with args.output.open('x') as stream:
        json.dump(result, stream, indent=2, ensure_ascii=False)
        stream.write('\n')
    print(json.dumps(result, ensure_ascii=False))
    return int(result['status'] != 'passed')


if __name__ == '__main__':
    raise SystemExit(main())
