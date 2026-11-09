"""Independent OOXML reader controls; these do not execute the application."""
import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest import mock
import warnings
import xml.etree.ElementTree as ET
import zipfile

SPEC = importlib.util.spec_from_file_location('reader', Path(__file__).resolve().parents[2] / 'scripts/verify_mobile_docx_export.py')
reader = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(reader)
MIXED = '  שָׁלוֹם ABC 123 مَرْحَبًا\tEND  '
EXPECTED = {'schema_version': 1, 'paragraphs': ['Document 2049', MIXED], 'mixed_rtl_paragraph': MIXED}


def parts(texts=None, rtl=True, preserve=True):
    document = ET.Element(reader.W + 'document')
    body = ET.SubElement(document, reader.W + 'body')
    for index, text in enumerate(texts or EXPECTED['paragraphs']):
        paragraph = ET.SubElement(body, reader.W + 'p')
        properties = ET.SubElement(paragraph, reader.W + 'pPr')
        ET.SubElement(properties, reader.W + 'bidi', {reader.W + 'val': '1' if index == 1 and rtl else '0'})
        run = ET.SubElement(paragraph, reader.W + 'r')
        for number, chunk in enumerate(text.split('\t')):
            if number:
                ET.SubElement(run, reader.W + 'tab')
            ET.SubElement(run, reader.W + 't', {reader.XML_SPACE: 'preserve'} if preserve else {}).text = chunk
    content = ET.Element(reader.CT + 'Types')
    ET.SubElement(content, reader.CT + 'Override', {'PartName': '/word/document.xml', 'ContentType': reader.WORD_TYPE})
    relations = ET.Element(reader.REL + 'Relationships')
    ET.SubElement(relations, reader.REL + 'Relationship', {'Id': 'rId1', 'Type': reader.OFFICE_REL, 'Target': 'word/document.xml'})
    return {'[Content_Types].xml': ET.tostring(content), '_rels/.rels': ET.tostring(relations),
            'word/document.xml': ET.tostring(document, encoding='utf-8')}


class WordReader(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.path = Path(self.temp.name) / 'review.docx'

    def write(self, values):
        with zipfile.ZipFile(self.path, 'w', zipfile.ZIP_DEFLATED) as archive:
            for name, data in values.items():
                archive.writestr(name, data)

    def test_exact_corrected_unicode_tabs_spaces_and_rtl_are_read(self):
        self.write(parts())
        result = reader.verify(self.path, EXPECTED)
        self.assertEqual(result['paragraphs'], EXPECTED['paragraphs'])
        self.assertEqual(result['paragraph_rtl'], [False, True])
        self.assertEqual(result['status'], 'passed')
        self.assertFalse(result['word_application_tested'])

    def test_changed_correction_missing_diacritics_and_reordered_text_fail(self):
        for texts in [['Document 2048', MIXED], ['Document 2049', MIXED.replace('שָׁלוֹם', 'שלום')], list(reversed(EXPECTED['paragraphs']))]:
            self.write(parts(texts))
            with self.assertRaisesRegex(ValueError, 'logical text/order'):
                reader.verify(self.path, EXPECTED)

    def test_missing_rtl_property_cannot_pass_from_text_alone(self):
        self.write(parts(rtl=False))
        with self.assertRaisesRegex(ValueError, 'RTL paragraph property'):
            reader.verify(self.path, EXPECTED)

    def test_missing_space_preservation_is_rejected_even_when_xml_text_matches(self):
        self.write(parts(preserve=False))
        with self.assertRaisesRegex(ValueError, 'xml:space'):
            reader.verify(self.path, EXPECTED)

    def test_duplicate_truncated_and_wrong_type_packages_fail(self):
        self.write(parts())
        with warnings.catch_warnings():
            warnings.simplefilter('ignore', UserWarning)
            with zipfile.ZipFile(self.path, 'a') as archive:
                archive.writestr('word/document.xml', parts()['word/document.xml'])
        with self.assertRaisesRegex(ValueError, 'Duplicate'):
            reader.verify(self.path, EXPECTED)
        self.write(parts())
        self.path.write_bytes(self.path.read_bytes()[:-32])
        with self.assertRaises(zipfile.BadZipFile):
            reader.verify(self.path, EXPECTED)
        values = parts()
        values['[Content_Types].xml'] = values['[Content_Types].xml'].replace(reader.WORD_TYPE.encode(), b'application/octet-stream')
        self.write(values)
        with self.assertRaisesRegex(ValueError, 'not a Word document'):
            reader.verify(self.path, EXPECTED)

    def test_uncompressed_budget_and_entity_declarations_fail_closed(self):
        self.write(parts())
        with mock.patch.object(reader, 'MAX_UNCOMPRESSED_BYTES', 32):
            with self.assertRaisesRegex(ValueError, 'uncompressed reader bound'):
                reader.verify(self.path, EXPECTED)
        values = parts()
        values['word/document.xml'] = b'<!DOCTYPE x [<!ENTITY x "value">]>' + values['word/document.xml']
        self.write(values)
        with self.assertRaisesRegex(ValueError, 'DTD/entity'):
            reader.verify(self.path, EXPECTED)


if __name__ == '__main__':
    unittest.main()
