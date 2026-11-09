#!/usr/bin/env python3
"""Independent ZIP/XML, python-docx, HTML and UTF-8 checks of real output."""
import hashlib
import json
import pathlib
import sys
import zipfile
from html.parser import HTMLParser
from xml.etree import ElementTree as ET

from docx import Document

root = pathlib.Path(sys.argv[1])
fixtures = json.loads((root / "fixtures.json").read_text())
namespace = "{http://schemas.openxmlformats.org/wordprocessingml/2006/main}"


class HtmlText(HTMLParser):
    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.pages, self.current = [], None
        self.viewport = False
        self.directions = []
        self.scripts = 0

    def handle_starttag(self, tag, attrs):
        attrs = dict(attrs)
        if tag == "section":
            self.pages.append([])
        if tag == "p":
            self.current = ""
            self.directions.append(attrs.get("dir"))
        if tag == "script":
            self.scripts += 1
        if tag == "meta" and attrs.get("name") == "viewport":
            self.viewport = "width=device-width" in attrs.get("content", "")

    def handle_endtag(self, tag):
        if tag == "p":
            self.pages[-1].append(self.current)
            self.current = None

    def handle_data(self, data):
        if self.current is not None:
            self.current += data


def truth(element):
    return element is not None and element.get(namespace + "val", "true") not in {"false", "0", "off"}


reports = []
for fixture in fixtures["cases"]:
    name = fixture["id"]
    pages = fixture["pages"]
    expected = ["\n".join(page) for page in pages]
    plain = (root / f"{name}.txt").read_bytes()
    assert plain == "\f".join(expected).encode("utf-8"), "Plain text changed reviewed Unicode or whitespace"

    html = HtmlText()
    html.feed((root / f"{name}.html").read_bytes().decode("utf-8"))
    assert html.pages == pages, "HTML changed the logical region text or page order"
    assert html.viewport and html.scripts == 0 and set(html.directions) == {"auto"}

    docx_path = root / f"{name}.docx"
    with zipfile.ZipFile(docx_path) as archive:
        assert archive.testzip() is None
        xml = archive.read("word/document.xml")
        assert not any(item.startswith("word/media/") for item in archive.namelist())
        for rels in (item for item in archive.namelist() if item.endswith(".rels")):
            assert not any(item.get("TargetMode") == "External" for item in ET.fromstring(archive.read(rels)))
    document = ET.fromstring(xml)
    parsed_pages, directions, paragraph_texts = [[]], [[]], []
    for paragraph in document.find(namespace + "body").findall(namespace + "p"):
        properties = paragraph.find(namespace + "pPr")
        assert properties is not None
        if truth(properties.find(namespace + "pageBreakBefore")):
            parsed_pages.append([])
            directions.append([])
        chunks = []
        for child in paragraph.iter():
            if child.tag == namespace + "t":
                assert child.get("{http://www.w3.org/XML/1998/namespace}space") == "preserve"
                chunks.append(child.text or "")
            elif child.tag == namespace + "tab":
                chunks.append("\t")
            elif child.tag in {namespace + "cr", namespace + "br"}:
                raise AssertionError("Unexpected Word line/page break encoding")
        text = "".join(chunks)
        parsed_pages[-1].append(text)
        paragraph_texts.append(text)
        directions[-1].append(truth(properties.find(namespace + "bidi")))
    word_expected = [text.replace("\r\n", "\n").replace("\r", "\n") for text in expected]
    assert ["\n".join(page) for page in parsed_pages] == word_expected, "OOXML changed text, blanks, tabs, semantic line breaks or pages"
    assert directions == fixture["paragraph_rtl"], "OOXML paragraph direction differs from independent first-strong ground truth"
    # A second library observes actual paragraph text. CR, LF and CRLF each
    # represent one Word paragraph boundary, including leading/trailing blanks.
    readback = [paragraph.text for paragraph in Document(docx_path).paragraphs]
    assert readback == [line for page in word_expected for line in page.split("\n")]
    files = {}
    for suffix in ["txt", "html", "docx"]:
        data = (root / f"{name}.{suffix}").read_bytes()
        files[suffix] = {"bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()}
    reports.append({"id": name, "pages": len(pages), "paragraphs": len(paragraph_texts),
                    "text_utf8_exact": True, "html_dom_source_exact": True,
                    "ooxml_logical_text_and_semantic_breaks_match": True, "python_docx_paragraphs_match": True,
                    "word_line_endings": "CR, LF and CRLF each become one paragraph boundary; no byte-exact line-ending claim",
                    "paragraph_rtl": directions, "files": files})
(root / "independent-readers.json").write_text(json.dumps(reports, ensure_ascii=False, indent=2) + "\n")
print(json.dumps(reports, ensure_ascii=False, indent=2))
