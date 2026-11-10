//! Literal, bounded XLSX export of explicitly selected reviewed OCR tables.
//!
//! This module receives current validated table data, never opens capture paths,
//! and never infers numeric values, dates, formulas, or additional worksheets.
//! Office string escaping follows MS-OI29500 §2.1.1747 (ST_Xstring): CR is
//! escaped, LF/tab stay literal in text, and a literal _xHHHH_ protects its lead
//! underscore. Sheet names are ST_Xstring attributes as well.
use crate::ocr_office::{validate_office_text, validate_table, OfficeTableData, MAX_OFFICE_OBJECTS,
    MAX_WORKBOOK_GRID_POSITIONS, MAX_WORKBOOK_TEXT_BYTES};
use std::collections::HashSet;
use std::fmt::Write as _;
use std::io::{self, Cursor, Seek, SeekFrom, Write};
use zip::write::SimpleFileOptions;

const MAX_XML_BYTES: usize = 128 * 1024 * 1024;
const MAX_PACKAGE_BYTES: usize = 128 * 1024 * 1024;
const SHEET_NS: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
const REL_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const PACKAGE_REL_NS: &str = "http://schemas.openxmlformats.org/package/2006/relationships";
const XML_DECL: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>";

#[derive(Clone, Debug)]
pub struct XlsxSheet {
    pub page_id: String,
    pub object_id: String,
    pub name: String,
    pub table: OfficeTableData,
}

#[derive(Debug)]
pub struct XlsxOutput {
    pub bytes: Vec<u8>,
    pub warnings: Vec<String>,
}

fn error(message: &str) -> String { format!("Cannot create reviewed Excel workbook: {message}.") }

/// Deliberately conservative, locale-independent collision key. This is the
/// same Unicode uppercase rule used by the UI's String.toUpperCase().
pub fn sheet_name_key(name: &str) -> String { name.to_uppercase() }

fn js_whitespace(ch: char) -> bool {
    matches!(ch, '\u{0009}'..='\u{000d}' | ' ' | '\u{00a0}' | '\u{1680}' |
        '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' |
        '\u{205f}' | '\u{3000}' | '\u{feff}')
}

/// Names are never trimmed, truncated, renamed, or deduplicated implicitly.
/// UTF-16 limits are conservative for readers that count surrogate pairs.
pub fn validate_sheet_name(name: &str) -> Result<(), String> {
    if name.chars().all(js_whitespace) || name.encode_utf16().count() > 31 {
        return Err(error("each worksheet needs a nonblank name of at most 31 characters (some Unicode characters count as two)"));
    }
    validate_office_text(name)?;
    if name.chars().any(|ch| ch.is_control() || matches!(ch, '/' | '\\' | '?' | '*' | ':' | '[' | ']')) {
        return Err(error("worksheet names cannot contain control characters or / \\ ? * : [ ]"));
    }
    if name.starts_with('\'') || name.ends_with('\'') {
        return Err(error("worksheet names cannot begin or end with an apostrophe"));
    }
    if sheet_name_key(name) == "HISTORY" {
        return Err(error("History is reserved by Excel; choose a different worksheet name"));
    }
    Ok(())
}

/// Validate original text before encoding. No newline conversion or numeric
/// interpretation occurs, and escaped XML size does not count as cell content.
pub fn validate_workbook_cell_text(text: &str) -> Result<(), String> {
    validate_office_text(text)?;
    if text.encode_utf16().count() > 32_767 {
        return Err(error("a cell exceeds 32,767 characters (some Unicode characters count as two); shorten or split it before exporting"));
    }
    if text.bytes().filter(|byte| *byte == b'\n').count() > 253 {
        return Err(error("a cell exceeds Excel's 253 line-feed limit; split it before exporting"));
    }
    Ok(())
}

fn valid_identity(value: &str) -> bool {
    !value.trim().is_empty() && value.encode_utf16().count() <= 200 && !value.contains('\0')
}

fn starts_xstring_escape(bytes: &[u8]) -> bool {
    bytes.len() >= 7 && bytes[0] == b'_' && bytes[1] == b'x' && bytes[6] == b'_' &&
        bytes[2..6].iter().all(u8::is_ascii_hexdigit)
}

fn needs_xstring_escape(text: &str) -> bool {
    text.contains('\r') || text.char_indices().any(|(offset, ch)| ch == '_' && starts_xstring_escape(&text.as_bytes()[offset..]))
}

/// One pass over the ORIGINAL input is essential: escape sequences we generate
/// must not be escaped again, and overlapping literal patterns remain literal.
fn escape_xstring_xml(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for (offset, ch) in text.char_indices() {
        match ch {
            '_' if starts_xstring_escape(&text.as_bytes()[offset..]) => escaped.push_str("_x005F_"),
            '\r' => escaped.push_str("_x000D_"),
            '&' => escaped.push_str("&amp;"), '<' => escaped.push_str("&lt;"), '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"), '\'' => escaped.push_str("&apos;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

fn validate_sheets(sheets: &[XlsxSheet]) -> Result<Vec<String>, String> {
    if sheets.is_empty() || sheets.len() > MAX_OFFICE_OBJECTS {
        return Err(error("explicitly select 1–512 tables for this workbook"));
    }
    let mut identities = HashSet::new();
    let mut names = HashSet::new();
    let mut grid_positions = 0usize;
    let mut text_bytes = 0usize;
    let mut has_escapes = false;
    let mut warnings = Vec::new();
    for sheet in sheets {
        if !valid_identity(&sheet.page_id) || !valid_identity(&sheet.object_id) ||
            !identities.insert((sheet.page_id.as_str(), sheet.object_id.as_str())) {
            return Err(error("each selected table needs a valid unique page and object identity"));
        }
        validate_sheet_name(&sheet.name)?;
        if !names.insert(sheet_name_key(&sheet.name)) {
            return Err(error("worksheet names must be unique without regard to case; no table was renamed"));
        }
        validate_table(&sheet.table).map_err(|failure| format!("Worksheet {:?}: {failure}", sheet.name))?;
        grid_positions = grid_positions.saturating_add(sheet.table.rows * sheet.table.columns);
        if grid_positions > MAX_WORKBOOK_GRID_POSITIONS {
            return Err(error("selected tables exceed 1,000,000 total grid positions; export fewer tables"));
        }
        has_escapes |= needs_xstring_escape(&sheet.name);
        for cell in &sheet.table.cells {
            validate_workbook_cell_text(&cell.text).map_err(|failure|
                format!("Worksheet {:?}, cell {}: {failure}", sheet.name, address(cell.row, cell.column)))?;
            text_bytes = text_bytes.saturating_add(cell.text.len());
            if text_bytes > MAX_WORKBOOK_TEXT_BYTES {
                return Err(error("selected cell text exceeds 64 MiB of UTF-8 data; export fewer tables"));
            }
            has_escapes |= needs_xstring_escape(&cell.text);
        }
        if sheet.table.rows == 0 {
            warnings.push(format!("Worksheet \"{}\" contains an explicitly selected 0 × 0 table and is exported empty.", sheet.name));
        }
    }
    if has_escapes {
        warnings.push("Carriage returns and literal _xHHHH_ sequences use standard OOXML string escapes. Readers without that decoding may display escape text; verify those values and names before re-saving in another application.".into());
    }
    Ok(warnings)
}

fn column_name(mut column: usize) -> String {
    // Table validation already bounds this to 0..1000; no Word 63-column limit.
    let mut reversed = Vec::new();
    loop {
        reversed.push(b'A' + (column % 26) as u8);
        if column < 26 { break; }
        column = column / 26 - 1;
    }
    reversed.reverse();
    String::from_utf8(reversed).expect("column coordinates contain only ASCII")
}

fn address(row: usize, column: usize) -> String { format!("{}{}", column_name(column), row + 1) }

fn worksheet_xml(table: &OfficeTableData) -> String {
    let mut xml = format!("{XML_DECL}<worksheet xmlns=\"{SHEET_NS}\">");
    if table.rows > 0 {
        write!(xml, "<dimension ref=\"A1:{}\"/>", address(table.rows - 1, table.columns - 1)).unwrap();
    }
    xml.push_str("<sheetViews><sheetView workbookViewId=\"0\"/></sheetViews><sheetFormatPr defaultRowHeight=\"15\"/>");
    if table.columns > 0 {
        write!(xml, "<cols><col min=\"1\" max=\"{}\" width=\"20\" customWidth=\"1\"/></cols>", table.columns).unwrap();
    }
    xml.push_str("<sheetData>");
    let mut cells = table.cells.iter().collect::<Vec<_>>();
    cells.sort_unstable_by_key(|cell| (cell.row, cell.column));
    let mut next_cell = 0;
    for row in 0..table.rows {
        write!(xml, "<row r=\"{}\">", row + 1).unwrap();
        while let Some(cell) = cells.get(next_cell).filter(|cell| cell.row == row) {
            write!(xml, "<c r=\"{}\" s=\"{}\" t=\"inlineStr\"><is><t xml:space=\"preserve\">{}</t></is></c>",
                address(cell.row, cell.column), usize::from(cell.header == Some(true)), escape_xstring_xml(&cell.text)).unwrap();
            next_cell += 1;
        }
        xml.push_str("</row>");
    }
    xml.push_str("</sheetData>");
    let merged = cells.iter().filter(|cell| cell.row_span > 1 || cell.column_span > 1).collect::<Vec<_>>();
    if !merged.is_empty() {
        write!(xml, "<mergeCells count=\"{}\">", merged.len()).unwrap();
        for cell in merged {
            write!(xml, "<mergeCell ref=\"{}:{}\"/>", address(cell.row, cell.column),
                address(cell.row + cell.row_span - 1, cell.column + cell.column_span - 1)).unwrap();
        }
        xml.push_str("</mergeCells>");
    }
    xml.push_str("</worksheet>");
    xml
}

fn workbook_xml(sheets: &[XlsxSheet]) -> String {
    let mut xml = format!("{XML_DECL}<workbook xmlns=\"{SHEET_NS}\" xmlns:r=\"{REL_NS}\"><bookViews><workbookView/></bookViews><sheets>");
    for (index, sheet) in sheets.iter().enumerate() {
        write!(xml, "<sheet name=\"{}\" sheetId=\"{}\" r:id=\"rId{}\"/>", escape_xstring_xml(&sheet.name), index + 1, index + 1).unwrap();
    }
    xml.push_str("</sheets></workbook>");
    xml
}

fn workbook_relationships(sheet_count: usize) -> String {
    let mut xml = format!("{XML_DECL}<Relationships xmlns=\"{PACKAGE_REL_NS}\">");
    for index in 1..=sheet_count {
        write!(xml, "<Relationship Id=\"rId{index}\" Type=\"{REL_NS}/worksheet\" Target=\"worksheets/sheet{index}.xml\"/>").unwrap();
    }
    write!(xml, "<Relationship Id=\"rId{}\" Type=\"{REL_NS}/styles\" Target=\"styles.xml\"/></Relationships>", sheet_count + 1).unwrap();
    xml
}

fn content_types(sheet_count: usize) -> String {
    let mut xml = format!("{XML_DECL}<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/><Default Extension=\"xml\" ContentType=\"application/xml\"/><Override PartName=\"/xl/workbook.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml\"/><Override PartName=\"/xl/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml\"/>");
    for index in 1..=sheet_count {
        write!(xml, "<Override PartName=\"/xl/worksheets/sheet{index}.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml\"/>").unwrap();
    }
    xml.push_str("</Types>");
    xml
}

fn styles_xml() -> String {
    format!("{XML_DECL}<styleSheet xmlns=\"{SHEET_NS}\">\
<fonts count=\"2\"><font><sz val=\"11\"/><name val=\"Calibri\"/><family val=\"2\"/></font><font><b/><sz val=\"11\"/><name val=\"Calibri\"/><family val=\"2\"/></font></fonts>\
<fills count=\"2\"><fill><patternFill patternType=\"none\"/></fill><fill><patternFill patternType=\"gray125\"/></fill></fills>\
<borders count=\"1\"><border><left/><right/><top/><bottom/><diagonal/></border></borders>\
<cellStyleXfs count=\"1\"><xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"0\"/></cellStyleXfs>\
<cellXfs count=\"2\"><xf numFmtId=\"49\" fontId=\"0\" fillId=\"0\" borderId=\"0\" xfId=\"0\" applyNumberFormat=\"1\" applyAlignment=\"1\"><alignment vertical=\"top\" wrapText=\"1\"/></xf><xf numFmtId=\"49\" fontId=\"1\" fillId=\"0\" borderId=\"0\" xfId=\"0\" applyNumberFormat=\"1\" applyAlignment=\"1\" applyFont=\"1\"><alignment vertical=\"top\" wrapText=\"1\"/></xf></cellXfs>\
<cellStyles count=\"1\"><cellStyle name=\"Normal\" xfId=\"0\" builtinId=\"0\"/></cellStyles></styleSheet>")
}

/// Cap writes while compression and ZIP headers are being produced, rather than
/// allocating an arbitrarily large archive and checking it only afterwards.
struct BoundedCursor { inner: Cursor<Vec<u8>>, limit: usize }
impl BoundedCursor {
    fn new(limit: usize) -> Self { Self { inner: Cursor::new(Vec::new()), limit } }
}
impl Write for BoundedCursor {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.inner.position().checked_add(bytes.len() as u64).is_none_or(|end| end > self.limit as u64) {
            return Err(io::Error::other("generated XLSX exceeds its package size limit"));
        }
        self.inner.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> { self.inner.flush() }
}
impl Seek for BoundedCursor {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        let target = match position {
            SeekFrom::Start(offset) => i128::from(offset),
            SeekFrom::Current(offset) => i128::from(self.inner.position()) + i128::from(offset),
            SeekFrom::End(offset) => self.inner.get_ref().len() as i128 + i128::from(offset),
        };
        if target < 0 || target > self.limit as i128 {
            return Err(io::Error::other("generated XLSX exceeds its package position limit"));
        }
        self.inner.set_position(target as u64);
        Ok(target as u64)
    }
}

fn add_part(archive: &mut zip::ZipWriter<BoundedCursor>, path: &str, xml: &str, xml_bytes: &mut usize) -> Result<(), String> {
    *xml_bytes = xml_bytes.saturating_add(xml.len());
    if *xml_bytes > MAX_XML_BYTES {
        return Err(error("generated worksheet XML exceeds 128 MiB; export fewer tables, with no values truncated"));
    }
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated)
        .compression_level(Some(6)).unix_permissions(0o644);
    archive.start_file(path, options).map_err(|failure| error(&format!("XLSX package creation failed: {failure}")))?;
    archive.write_all(xml.as_bytes()).map_err(|failure| error(&format!("XLSX package creation failed: {failure}")))
}

/// One explicitly named worksheet per selected current table, in request order.
/// Merged cells retain text solely in their existing top-left anchor. Original
/// source boxes and model history remain in the project/JSON, not invented cells.
pub fn render_xlsx(sheets: &[XlsxSheet]) -> Result<XlsxOutput, String> {
    let warnings = validate_sheets(sheets)?;
    let mut archive = zip::ZipWriter::new(BoundedCursor::new(MAX_PACKAGE_BYTES));
    let mut xml_bytes = 0;
    add_part(&mut archive, "[Content_Types].xml", &content_types(sheets.len()), &mut xml_bytes)?;
    let relationships = format!("{XML_DECL}<Relationships xmlns=\"{PACKAGE_REL_NS}\"><Relationship Id=\"rId1\" Type=\"{REL_NS}/officeDocument\" Target=\"xl/workbook.xml\"/></Relationships>");
    add_part(&mut archive, "_rels/.rels", &relationships, &mut xml_bytes)?;
    add_part(&mut archive, "xl/workbook.xml", &workbook_xml(sheets), &mut xml_bytes)?;
    add_part(&mut archive, "xl/_rels/workbook.xml.rels", &workbook_relationships(sheets.len()), &mut xml_bytes)?;
    add_part(&mut archive, "xl/styles.xml", &styles_xml(), &mut xml_bytes)?;
    for (index, sheet) in sheets.iter().enumerate() {
        add_part(&mut archive, &format!("xl/worksheets/sheet{}.xml", index + 1), &worksheet_xml(&sheet.table), &mut xml_bytes)?;
    }
    let cursor = archive.finish().map_err(|failure| error(&format!("XLSX package creation failed: {failure}")))?;
    Ok(XlsxOutput { bytes: cursor.inner.into_inner(), warnings })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ocr_office::{OfficeBounds, OfficeCellBox, OfficeTableCell};
    use quick_xml::events::Event;
    use std::collections::HashMap;
    use std::io::Read;

    fn cell(id: &str, row: usize, column: usize, text: &str) -> OfficeTableCell {
        OfficeTableCell { id: id.into(), row, column, row_span: 1, column_span: 1,
            text: text.into(), header: None, source_box: None }
    }
    fn row(values: &[&str]) -> OfficeTableData {
        OfficeTableData { version: 1, rows: usize::from(!values.is_empty()), columns: values.len(),
            cells: values.iter().enumerate().map(|(column, text)| cell(&format!("cell-{column}"), 0, column, text)).collect() }
    }
    fn sheet(id: &str, name: &str, table: OfficeTableData) -> XlsxSheet {
        XlsxSheet { page_id: "page".into(), object_id: id.into(), name: name.into(), table }
    }
    fn package(bytes: &[u8]) -> HashMap<String, String> {
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        let mut parts = HashMap::new();
        for index in 0..archive.len() {
            let mut part = archive.by_index(index).unwrap();
            assert!(!part.is_dir());
            let mut xml = String::new();
            part.read_to_string(&mut xml).unwrap();
            assert!(parts.insert(part.name().into(), xml).is_none(), "duplicate ZIP part");
        }
        parts
    }

    #[derive(Debug)]
    struct Element { name: String, attributes: HashMap<String, String>, children: Vec<Element>, text: String }
    impl Element {
        fn attribute(&self, name: &str) -> &str { self.attributes.get(name).unwrap().as_str() }
        fn child(&self, name: &str) -> &Element {
            let mut found = self.children.iter().filter(|child| child.name == name);
            let child = found.next().unwrap();
            assert!(found.next().is_none(), "duplicate {name} in {self:?}");
            child
        }
        fn descendants<'a>(&'a self, name: &str) -> Vec<&'a Element> {
            let mut found = if self.name == name { vec![self] } else { Vec::new() };
            for child in &self.children { found.extend(child.descendants(name)); }
            found
        }
    }
    fn parse_xml(text: &str) -> Element {
        let mut reader = quick_xml::Reader::from_str(text);
        let mut stack: Vec<Element> = Vec::new();
        let mut root = None;
        loop {
            let event = reader.read_event().unwrap();
            let empty = matches!(&event, Event::Empty(_));
            match event {
                Event::Start(start) | Event::Empty(start) => {
                    let node = Element {
                        name: String::from_utf8(start.name().as_ref().to_vec()).unwrap(),
                        attributes: start.attributes().map(|attribute| {
                            let attribute = attribute.unwrap();
                            (String::from_utf8(attribute.key.as_ref().to_vec()).unwrap(),
                                attribute.decode_and_unescape_value(reader.decoder()).unwrap().into_owned())
                        }).collect(), children: Vec::new(), text: String::new(),
                    };
                    if empty {
                        if let Some(parent) = stack.last_mut() { parent.children.push(node); }
                        else { assert!(root.replace(node).is_none()); }
                    } else { stack.push(node); }
                },
                Event::End(_) => {
                    let node = stack.pop().unwrap();
                    if let Some(parent) = stack.last_mut() { parent.children.push(node); }
                    else { assert!(root.replace(node).is_none()); }
                },
                Event::Text(text) => stack.last_mut().unwrap().text.push_str(&text.xml_content().unwrap()),
                Event::GeneralRef(reference) => {
                    let ch = reference.resolve_char_ref().unwrap().unwrap_or_else(|| match reference.decode().unwrap().as_ref() {
                        "amp" => '&', "lt" => '<', "gt" => '>', "quot" => '"', "apos" => '\'', other => panic!("unexpected XML entity {other}"),
                    });
                    stack.last_mut().unwrap().text.push(ch);
                },
                Event::Decl(_) => (), Event::Eof => break,
                other => panic!("unexpected workbook XML event {other:?}"),
            }
        }
        assert!(stack.is_empty());
        root.unwrap()
    }

    // An independent one-pass decoder for XML-decoded ST_Xstring content.
    // It consumes a complete escape once; decoded underscores are not rescanned.
    fn decode_xstring(text: &str) -> String {
        let mut units = Vec::new();
        let mut offset = 0;
        while offset < text.len() {
            let remaining = &text[offset..];
            let encoded = remaining.as_bytes().get(..7).filter(|bytes|
                bytes.starts_with(b"_x") && bytes[6] == b'_' && bytes[2..6].iter().all(u8::is_ascii_hexdigit));
            if let Some(encoded) = encoded {
                units.push(u16::from_str_radix(std::str::from_utf8(&encoded[2..6]).unwrap(), 16).unwrap());
                offset += 7;
            } else {
                let ch = remaining.chars().next().unwrap();
                units.extend(ch.encode_utf16(&mut [0; 2]).iter().copied());
                offset += ch.len_utf8();
            }
        }
        String::from_utf16(&units).unwrap()
    }
    fn literal_cells(worksheet: &Element) -> Vec<(String, String)> {
        worksheet.descendants("c").into_iter().map(|cell| {
            assert_eq!(cell.attribute("t"), "inlineStr");
            assert_eq!(cell.children.len(), 1, "no formula or inferred numeric value may accompany text");
            let text = cell.child("is").child("t");
            assert_eq!(text.attribute("xml:space"), "preserve");
            (cell.attribute("r").into(), decode_xstring(&text.text))
        }).collect()
    }

    #[test]
    fn sheet_names_are_preserved_and_reject_invalid_or_ambiguous_limits() {
        for name in ["Reviewed table", "O'Brien", " A & \"B\" ", "日本語", "_x000D_", "😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀A"] {
            validate_sheet_name(name).unwrap();
        }
        validate_sheet_name(&"a".repeat(31)).unwrap();
        for name in ["", " \u{feff}\u{00a0}", "a/b", "a\\b", "a?b", "a*b", "a:b", "a[b", "a]b", "'a", "a'",
            "history", "HiStOrY", "a\tb", "a\rb", "a\nb", "a\u{007f}b", "a\u{0085}b", "a\u{ffff}b"] {
            assert!(validate_sheet_name(name).is_err(), "accepted invalid name {name:?}");
        }
        assert!(validate_sheet_name(&"a".repeat(32)).is_err());
        assert!(validate_sheet_name(&"😀".repeat(16)).is_err());
        assert_eq!(sheet_name_key("Straße"), sheet_name_key("STRASSE"));
        assert_eq!(sheet_name_key("ς"), sheet_name_key("σ"));
        assert_eq!(sheet_name_key("ﬃ"), "FFI");
    }

    #[test]
    fn cell_limits_count_original_unicode_and_line_feeds_without_normalization() {
        validate_workbook_cell_text(&"a".repeat(32_767)).unwrap();
        validate_workbook_cell_text(&format!("{}a", "😀".repeat(16_383))).unwrap();
        validate_workbook_cell_text(&"\r\n".repeat(253)).unwrap();
        validate_workbook_cell_text("\t\n\r😀 e\u{301} العربية").unwrap();
        assert!(validate_workbook_cell_text(&"a".repeat(32_768)).unwrap_err().contains("32,767"));
        assert!(validate_workbook_cell_text(&"😀".repeat(16_384)).is_err());
        assert!(validate_workbook_cell_text(&"\n".repeat(254)).unwrap_err().contains("253"));
        for text in ["nul\0", "bell\u{7}", "vertical\u{b}", "form\u{c}", "\u{fffe}", "\u{ffff}"] {
            assert!(validate_workbook_cell_text(text).is_err(), "accepted XML-invalid {text:?}");
        }
        let mut table = row(&["Good", "Too long"]);
        table.cells[1].text = "a".repeat(32_768);
        let message = render_xlsx(&[sheet("long", "Reviewed invoices", table)]).unwrap_err();
        assert!(message.contains("Reviewed invoices") && message.contains("B1") && message.contains("32,767"));
    }

    #[test]
    fn selection_identity_and_case_insensitive_names_are_unique() {
        let a = sheet("table", "Table", row(&["A"]));
        let mut duplicate = a.clone(); duplicate.name = "Another".into();
        assert!(render_xlsx(&[a.clone(), duplicate]).unwrap_err().contains("identity"));
        let collision = sheet("table-two", "tAbLe", row(&["B"]));
        assert!(render_xlsx(&[a.clone(), collision]).unwrap_err().contains("unique without regard to case"));
        let unicode = [sheet("one", "Straße", row(&["A"])), sheet("two", "STRASSE", row(&["B"]))];
        assert!(render_xlsx(&unicode).is_err());
        let mut other_page = a.clone(); other_page.page_id = "page-two".into(); other_page.name = "Second page".into();
        render_xlsx(&[a, other_page]).unwrap();
        for invalid in ["", " ", "bad\0id"] {
            let mut source = sheet("id", "Name", row(&["value"])); source.page_id = invalid.into();
            assert!(render_xlsx(&[source]).is_err());
        }
    }

    #[test]
    fn package_relationships_types_and_sheet_order_resolve_to_exact_selected_data() {
        let sources = [sheet("later", "B & \"quoted\"", row(&["SECOND INPUT FIRST"])),
            sheet("earlier", "O'Brien", row(&["FIRST INPUT SECOND"]))];
        let output = render_xlsx(&sources).unwrap();
        let parts = package(&output.bytes);
        assert_eq!(parts.len(), 7);
        let root = parse_xml(&parts["_rels/.rels"]);
        let office = root.child("Relationship");
        assert_eq!(office.attribute("Type"), format!("{REL_NS}/officeDocument"));
        assert_eq!(office.attribute("Target"), "xl/workbook.xml");
        let workbook = parse_xml(&parts[office.attribute("Target")]);
        assert_eq!(workbook.attribute("xmlns"), SHEET_NS);
        let relationships = parse_xml(&parts["xl/_rels/workbook.xml.rels"]);
        let mut by_id = HashMap::new();
        for relationship in &relationships.children {
            assert!(relationship.attributes.get("TargetMode").is_none());
            assert!(by_id.insert(relationship.attribute("Id"), relationship).is_none());
            assert!(parts.contains_key(&format!("xl/{}", relationship.attribute("Target"))));
        }
        let tabs = &workbook.child("sheets").children;
        assert_eq!(tabs.len(), sources.len());
        let mut sheet_ids = HashSet::new();
        for (tab, source) in tabs.iter().zip(&sources) {
            assert_eq!(decode_xstring(tab.attribute("name")), source.name);
            assert!(sheet_ids.insert(tab.attribute("sheetId")));
            let relationship = by_id[tab.attribute("r:id")];
            assert_eq!(relationship.attribute("Type"), format!("{REL_NS}/worksheet"));
            let worksheet = parse_xml(&parts[&format!("xl/{}", relationship.attribute("Target"))]);
            assert_eq!(literal_cells(&worksheet), vec![("A1".into(), source.table.cells[0].text.clone())]);
        }
        let types = parse_xml(&parts["[Content_Types].xml"]);
        let mut overrides = HashSet::new();
        for part in types.children.iter().filter(|part| part.name == "Override") {
            assert!(overrides.insert(part.attribute("PartName")));
            assert!(parts.contains_key(part.attribute("PartName").strip_prefix('/').unwrap()));
        }
        assert_eq!(overrides.len(), 4);
        assert_eq!(by_id.values().filter(|relationship| relationship.attribute("Type") == format!("{REL_NS}/styles")).count(), 1);
        assert!(output.warnings.is_empty());
    }

    #[test]
    fn literal_strings_survive_as_exact_inline_text_without_formula_or_number_inference() {
        let values = ["0012", "-001.0", "€1,250.00", "12/10/24", "=SUM(A1:A3)", "+2", "-0", "@literal",
            "  leading and trailing  ", "\tA\nB\n\n", "", "😀 e\u{301} العربية", "<tag>&\"quote\"'", "TRUE"];
        let source = sheet("values", "Literal values", row(&values));
        let before = source.table.clone();
        let output = render_xlsx(std::slice::from_ref(&source)).unwrap();
        assert_eq!(source.table, before);
        let parts = package(&output.bytes);
        let worksheet = parse_xml(&parts["xl/worksheets/sheet1.xml"]);
        assert!(worksheet.descendants("f").is_empty() && worksheet.descendants("v").is_empty());
        let actual = literal_cells(&worksheet).into_iter().map(|(_, text)| text).collect::<Vec<_>>();
        assert_eq!(actual, values);
        assert!(!parts.keys().any(|part| part.contains("sharedStrings") || part.contains("externalLink")));
    }

    #[test]
    fn xstring_encoding_is_single_pass_for_cr_literal_and_overlapping_escape_shapes() {
        let values = ["A\r\nB\rC\nD\tE", "_x000D_", "_x005F_", "_x005F_x000D_", "_x000D__x000A_",
            "_x00aD_", "_X000D_", "_xFFF_", "_x0000_", "_xD83D__xDE00_", "😀_x005F_\r<&>"];
        let source = sheet("escapes", "_x000D_ & notes", row(&values));
        let output = render_xlsx(&[source]).unwrap();
        let parts = package(&output.bytes);
        let worksheet = parse_xml(&parts["xl/worksheets/sheet1.xml"]);
        let actual = literal_cells(&worksheet).into_iter().map(|(_, text)| text).collect::<Vec<_>>();
        assert_eq!(actual, values);
        let cells = worksheet.descendants("c");
        assert_eq!(cells[0].child("is").child("t").text, "A_x000D_\nB_x000D_C\nD\tE");
        assert_eq!(cells[1].child("is").child("t").text, "_x005F_x000D_");
        assert_eq!(cells[2].child("is").child("t").text, "_x005F_x005F_");
        assert_eq!(cells[3].child("is").child("t").text, "_x005F_x005F_x005F_x000D_");
        assert_eq!(cells[6].child("is").child("t").text, "_X000D_");
        let workbook = parse_xml(&parts["xl/workbook.xml"]);
        let name = workbook.child("sheets").child("sheet").attribute("name");
        assert_eq!(name, "_x005F_x000D_ & notes");
        assert_eq!(decode_xstring(name), "_x000D_ & notes");
        assert!(output.warnings.iter().any(|warning| warning.contains("OOXML string escapes")));
    }

    #[test]
    fn merged_anchors_and_header_styles_resolve_without_duplicating_or_moving_text() {
        let mut anchor = cell("merged", 0, 0, "Merged\n0012");
        anchor.row_span = 2; anchor.column_span = 2; anchor.header = Some(true);
        anchor.source_box = Some(OfficeCellBox { capture_id: "capture".into(), bounds: OfficeBounds { x: 10.0, y: 20.0, w: 30.0, h: 40.0 } });
        let mut table = OfficeTableData { version: 1, rows: 3, columns: 3, cells: vec![anchor,
            cell("c1", 0, 2, "A"), cell("c2", 1, 2, "B"), cell("a3", 2, 0, "C"), cell("b3", 2, 1, ""), cell("c3", 2, 2, "E")] };
        table.cells.reverse(); // Serialization orders anchors, never the input data.
        let source = sheet("merged", "Merged", table.clone());
        let output = render_xlsx(std::slice::from_ref(&source)).unwrap();
        assert_eq!(source.table, table);
        let parts = package(&output.bytes);
        let worksheet = parse_xml(&parts["xl/worksheets/sheet1.xml"]);
        let cells = literal_cells(&worksheet);
        assert_eq!(cells, vec![("A1".into(), "Merged\n0012".into()), ("C1".into(), "A".into()), ("C2".into(), "B".into()),
            ("A3".into(), "C".into()), ("B3".into(), "".into()), ("C3".into(), "E".into())]);
        let merges = worksheet.child("mergeCells");
        assert_eq!(merges.attribute("count"), "1");
        assert_eq!(merges.child("mergeCell").attribute("ref"), "A1:B2");
        let styles = parse_xml(&parts["xl/styles.xml"]);
        let fonts = &styles.child("fonts").children;
        let formats = &styles.child("cellXfs").children;
        for node in worksheet.descendants("c") {
            let style = &formats[node.attribute("s").parse::<usize>().unwrap()];
            assert_eq!(style.attribute("numFmtId"), "49");
            assert_eq!(style.child("alignment").attribute("wrapText"), "1");
            let font = &fonts[style.attribute("fontId").parse::<usize>().unwrap()];
            assert_eq!(!font.descendants("b").is_empty(), node.attribute("r") == "A1");
        }
    }

    #[test]
    fn wide_and_tall_tables_use_real_excel_coordinates_without_word_limits() {
        let table = OfficeTableData { version: 1, rows: 1, columns: 1_000,
            cells: (0..1_000).map(|column| cell(&format!("cell-{column}"), 0, column, &format!("{column:04}"))).collect() };
        let output = render_xlsx(&[sheet("wide", "Wide", table)]).unwrap();
        let parts = package(&output.bytes);
        let worksheet = parse_xml(&parts["xl/worksheets/sheet1.xml"]);
        assert_eq!(worksheet.child("dimension").attribute("ref"), "A1:ALL1");
        let cells = literal_cells(&worksheet);
        for (index, coordinate) in [(25, "Z1"), (26, "AA1"), (51, "AZ1"), (52, "BA1"), (63, "BL1"), (999, "ALL1")] {
            assert_eq!(cells[index], (coordinate.into(), format!("{index:04}")));
        }
        let mut tall_cell = cell("tall", 0, 0, "Entire span"); tall_cell.row_span = 1_000;
        let output = render_xlsx(&[sheet("tall", "Tall", OfficeTableData { version: 1, rows: 1_000, columns: 1, cells: vec![tall_cell] })]).unwrap();
        let parts = package(&output.bytes);
        let tall = parse_xml(&parts["xl/worksheets/sheet1.xml"]);
        assert_eq!(tall.child("dimension").attribute("ref"), "A1:A1000");
        assert_eq!(tall.child("mergeCells").child("mergeCell").attribute("ref"), "A1:A1000");
        assert_eq!(literal_cells(&tall), vec![("A1".into(), "Entire span".into())]);
    }

    #[test]
    fn explicitly_selected_empty_table_remains_a_named_empty_worksheet() {
        let output = render_xlsx(&[sheet("empty", "Empty reviewed table", row(&[]))]).unwrap();
        let parts = package(&output.bytes);
        let workbook = parse_xml(&parts["xl/workbook.xml"]);
        assert_eq!(workbook.child("sheets").child("sheet").attribute("name"), "Empty reviewed table");
        let worksheet = parse_xml(&parts["xl/worksheets/sheet1.xml"]);
        assert!(worksheet.child("sheetData").children.is_empty());
        assert!(worksheet.descendants("c").is_empty() && worksheet.descendants("dimension").is_empty());
        assert!(output.warnings.iter().any(|warning| warning.contains("0 × 0") && warning.contains("Empty reviewed table")));
    }

    #[test]
    fn malformed_grids_and_xml_invalid_cells_fail_before_a_package_is_returned() {
        let mut bad_version = row(&["A"]); bad_version.version = 2;
        let mut missing = row(&["A", "B"]); missing.cells.pop();
        let mut overlap = row(&["A", "B"]); overlap.cells[1].column = 0;
        let mut overflow = row(&["A"]); overflow.cells[0].column_span = usize::MAX;
        let mut invalid_empty = row(&[]); invalid_empty.rows = 1;
        let mut invalid_text = row(&["A"]); invalid_text.cells[0].text = "\u{000b}".into();
        let mut too_wide = row(&[]); too_wide.rows = 1; too_wide.columns = 1_001;
        for table in [bad_version, missing, overlap, overflow, invalid_empty, invalid_text, too_wide] {
            assert!(render_xlsx(&[sheet("bad", "Bad", table)]).is_err());
        }
    }

    #[test]
    fn workbook_sheet_and_aggregate_grid_limits_are_enforced_before_serialization() {
        assert!(render_xlsx(&[]).is_err());
        let sheets = (0..513).map(|index| sheet(&format!("table-{index}"), &format!("Sheet {index}"), row(&[]))).collect::<Vec<_>>();
        assert!(render_xlsx(&sheets).unwrap_err().contains("1–512"));
        assert_eq!(validate_sheets(&sheets[..512]).unwrap().len(), 512);
        let mut span = cell("span", 0, 0, ""); span.row_span = 100; span.column_span = 100;
        let table = OfficeTableData { version: 1, rows: 100, columns: 100, cells: vec![span] };
        let grids = (0..101).map(|index| sheet(&format!("grid-{index}"), &format!("Grid {index}"), table.clone())).collect::<Vec<_>>();
        validate_sheets(&grids[..100]).unwrap();
        assert!(render_xlsx(&grids).unwrap_err().contains("1,000,000 total grid positions"));
    }

    #[test]
    fn aggregate_utf8_budget_is_distinct_from_per_cell_and_per_table_limits() {
        // Every cell and table is legal, but CJK takes three UTF-8 bytes per
        // UTF-16 unit. An aggregate character count would miss this boundary.
        let text = "界".repeat(32_767);
        let table = OfficeTableData { version: 1, rows: 1, columns: 30,
            cells: (0..30).map(|column| cell(&format!("cell-{column}"), 0, column, &text)).collect() };
        validate_table(&table).unwrap();
        let sheets = (0..23).map(|index| sheet(&format!("text-{index}"), &format!("Text {index}"), table.clone())).collect::<Vec<_>>();
        assert!(validate_sheets(&sheets).unwrap_err().contains("64 MiB of UTF-8"));
    }

    #[test]
    fn zip_and_xml_limits_stop_writes_without_returning_partial_content() {
        let mut sink = BoundedCursor::new(8);
        sink.write_all(b"12345678").unwrap();
        assert!(sink.write_all(b"9").is_err());
        assert_eq!(sink.inner.get_ref(), b"12345678");
        sink.seek(SeekFrom::Start(2)).unwrap();
        sink.write_all(b"AB").unwrap();
        assert_eq!(sink.inner.get_ref(), b"12AB5678");
        assert!(sink.seek(SeekFrom::End(1)).is_err());
        assert!(sink.seek(SeekFrom::Current(-9)).is_err());
        let mut archive = zip::ZipWriter::new(BoundedCursor::new(MAX_PACKAGE_BYTES));
        let mut xml_bytes = MAX_XML_BYTES;
        assert!(add_part(&mut archive, "oversize.xml", "<x/>", &mut xml_bytes).unwrap_err().contains("128 MiB"));
    }
}
