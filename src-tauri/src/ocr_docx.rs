//! Reflowable DOCX from reviewed OCR content, independent of Tauri and paths.
//!
//! The workspace validates managed source images and supplies cropped PNG bytes.
//! This module never opens source/provenance paths and never reruns recognition.
//! It deliberately targets the application's pinned docx-rs 0.4.20 API.
use crate::ocr_document::{validate_structure, DocumentStructure, InlineRun, StructureBlock, StructureSection, StructureSource};
use crate::ocr_office::{validate_office_text, validate_table, OfficeExportObject, OfficeObjectData, OfficePlacement, OfficeTableData, MAX_OFFICE_OBJECTS};
use docx_rs::*;
use std::collections::{HashMap, HashSet};
use std::io::Cursor;

const MAX_WORD_POINTS: f64 = 1584.0;
const MAX_TEXT_BYTES: usize = 64 * 1024 * 1024;
const MAX_IMAGE_BYTES: usize = 64 * 1024 * 1024;
// docx-rs 0.4.20 Numberings::build_to always emits abstractNumId=1 and
// numId=1, even for Numberings::new(). Reserve both namespaces explicitly;
// duplicate IDs let readers resolve our list paragraphs to the built-in list.
const DOCX_RS_BUILT_IN_NUMBERING_ID: usize = 1;
const BULLET_ABSTRACT_ID: usize = 2;
const DECIMAL_ABSTRACT_ID: usize = 3;

#[derive(Clone, Debug)]
pub struct DocxRegion {
    pub id: String,
    pub kind: String,
    pub text: String,
    pub separator_before: Option<String>,
}

/// PNG bytes are prepared from a validated current managed image, never a path
/// copied from extraction history. Display dimensions are points, not OCR boxes.
#[derive(Clone, Debug)]
pub struct DocxFigure {
    pub region_id: String,
    pub png: Vec<u8>,
    pub width_px: u32,
    pub height_px: u32,
    pub width_points: f64,
    pub height_points: f64,
}

#[derive(Clone, Debug)]
pub struct DocxPage {
    pub id: String,
    pub page_width_points: f64,
    pub page_height_points: f64,
    pub regions: Vec<DocxRegion>,
    pub figures: Vec<DocxFigure>,
    pub structure: Option<DocumentStructure>,
    pub objects: Vec<OfficeExportObject>,
}

#[derive(Debug)]
pub struct DocxOutput {
    pub bytes: Vec<u8>,
    pub warnings: Vec<String>,
}

fn error(message: &str) -> String { format!("Cannot create reviewed Word document: {message}.") }
fn valid_id(value: &str) -> bool {
    !value.trim().is_empty() && value.encode_utf16().count() <= 200 && !value.contains('\0')
}
fn warn(warnings: &mut Vec<String>, message: &str) {
    if !warnings.iter().any(|warning| warning == message) { warnings.push(message.to_owned()); }
}

fn add_text_budget(text: &str, total: &mut usize, warnings: &mut Vec<String>) -> Result<(), String> {
    validate_office_text(text)?;
    *total = total.saturating_add(text.len());
    if *total > MAX_TEXT_BYTES { return Err(error("reviewed text exceeds 64 MB; no text was truncated")); }
    if text.contains('\r') {
        warn(warnings, "CRLF and lone CR characters are represented as single Word line breaks. Their visible lines are preserved; original newline bytes remain in the project and JSON.");
    }
    Ok(())
}

fn default_structure(page: &DocxPage) -> DocumentStructure {
    let blocks = page.regions.iter().enumerate().map(|(index, region)| {
        let role = match region.kind.as_str() {
            "title" => "heading", "table" => "table", "formula" => "formula",
            "header" => "header", "footer" => "footer", _ => "paragraph",
        };
        StructureBlock {
            id: format!("docx-block-{index}"), source_region_id: region.id.clone(),
            section_id: "docx-section".into(), column: 1, role: role.into(),
            heading_level: (role == "heading").then_some(1), list: None,
            alignment: "start".into(), break_before: "none".into(), runs: vec![],
            anchor_block_id: None, note_references: None, alt_text: None, decorative: None,
        }
    }).collect::<Vec<_>>();
    DocumentStructure { version: 1,
        sections: if blocks.is_empty() { vec![] } else { vec![StructureSection { id: "docx-section".into(), columns: 1 }] },
        blocks,
    }
}

fn validate_pages(pages: &[DocxPage], warnings: &mut Vec<String>) -> Result<(), String> {
    if pages.is_empty() || pages.len() > 10_000 { return Err(error("select 1–10,000 source pages")); }
    let mut page_ids = HashSet::new();
    let mut text_bytes = 0usize;
    let mut image_bytes = 0usize;
    let mut object_count = 0usize;
    for page in pages {
        if !valid_id(&page.id) || !page_ids.insert(page.id.as_str()) { return Err(error("source page identities are invalid or repeated")); }
        if [page.page_width_points, page.page_height_points].iter().any(|size| !size.is_finite() || *size <= 0.0 || *size > MAX_WORD_POINTS || (*size * 20.0).round() < 1.0) {
            return Err(error("Word page dimensions must be positive and no greater than 1584 points (22 inches). Choose a smaller page size before DOCX export"));
        }
        if page.regions.len().saturating_add(page.figures.len()) > 100_000 { return Err(error("a page exceeds 100,000 source regions")); }
        let mut ids = HashSet::new();
        for region in &page.regions {
            if !valid_id(&region.id) || !ids.insert(region.id.as_str()) ||
                !["text", "title", "header", "footer", "table", "formula"].contains(&region.kind.as_str()) {
                return Err(error("source regions need unique identities and supported text kinds"));
            }
            if region.separator_before.as_ref().is_some_and(|value| !["", " ", "\n", "\n\n"].contains(&value.as_str())) {
                return Err(error("a source passage separator is unsupported"));
            }
            add_text_budget(&region.text, &mut text_bytes, warnings)?;
        }
        for figure in &page.figures {
            if !valid_id(&figure.region_id) || !ids.insert(figure.region_id.as_str()) || figure.width_px == 0 || figure.height_px == 0 ||
                figure.width_px > 100_000 || figure.height_px > 100_000 || u64::from(figure.width_px) * u64::from(figure.height_px) > 100_000_000 ||
                [figure.width_points, figure.height_points].iter().any(|size| !size.is_finite() || *size <= 0.0 || *size > 14_400.0) {
                return Err(error("a prepared figure has invalid identity or dimensions"));
            }
            // The adapter encodes these PNGs. Check their measured identity again
            // without decoding a second 100-million-pixel allocation here.
            if figure.png.len() < 33 || figure.png[..8] != [137, 80, 78, 71, 13, 10, 26, 10] ||
                &figure.png[12..16] != b"IHDR" || figure.png[8..12] != [0, 0, 0, 13] ||
                u32::from_be_bytes(figure.png[16..20].try_into().unwrap()) != figure.width_px ||
                u32::from_be_bytes(figure.png[20..24].try_into().unwrap()) != figure.height_px {
                return Err(error("prepared figure PNG dimensions do not match their capture"));
            }
            image_bytes = image_bytes.saturating_add(figure.png.len());
            if image_bytes > MAX_IMAGE_BYTES { return Err(error("prepared figure PNGs exceed 64 MB; no image was omitted")); }
        }
        if !page.figures.is_empty() && page.structure.is_none() { return Err(error("picture order requires an explicit document structure")); }
        let fallback;
        let structure = if let Some(structure) = &page.structure { structure } else { fallback = default_structure(page); &fallback };
        let text_sources = page.regions.iter().map(|region| StructureSource { id: &region.id, text: &region.text, kind: &region.kind }).collect::<Vec<_>>();
        let figures = page.figures.iter().map(|figure| StructureSource { id: &figure.region_id, text: "", kind: "picture" }).collect::<Vec<_>>();
        validate_structure(structure, &text_sources, &figures)?;
        let mut selected = HashSet::new();
        let mut replacements = HashSet::new();
        object_count = object_count.saturating_add(page.objects.len());
        if object_count > MAX_OFFICE_OBJECTS { return Err(error("too many selected extraction objects")); }
        for object in &page.objects {
            if !valid_id(&object.object_id) || !selected.insert(object.object_id.as_str()) || object.name.encode_utf16().count() > 1000 || object.name.contains('\0') {
                return Err(error("selected extraction object identities or names are invalid"));
            }
            let kind = match &object.data {
                OfficeObjectData::Table(table) => {
                    validate_table(table)?;
                    if table.rows == 0 { return Err(error("an empty 0 by 0 table cannot be represented as an editable Word table; deselect it or add a row first")); }
                    if table.columns > 63 { return Err(error("Word supports at most 63 logical table columns. Choose CSV or JSON for a wider table")); }
                    for cell in &table.cells { add_text_budget(&cell.text, &mut text_bytes, warnings)?; }
                    "table"
                },
                OfficeObjectData::Formula(latex) => {
                    if latex.encode_utf16().count() > 1_000_000 { return Err(error("formula text exceeds 1,000,000 units")); }
                    add_text_budget(latex, &mut text_bytes, warnings)?;
                    "formula"
                },
            };
            if let OfficePlacement::ReplaceRegion { region_id } = &object.placement {
                if !replacements.insert(region_id.as_str()) || !page.regions.iter().any(|region| &region.id == region_id && region.kind == kind) {
                    return Err(error("each replacement needs a unique included source region of the same kind"));
                }
            }
        }
    }
    Ok(())
}

/// Text control characters become actual Word controls. Neither whitespace nor
/// source punctuation is trimmed, interpreted as markup, or guessed as a value.
fn literal_run(text: &str, marks: Option<&InlineRun>) -> Run {
    let mut run = Run::new();
    if let Some(marks) = marks {
        if marks.bold == Some(true) { run = run.bold(); }
        if marks.italic == Some(true) { run = run.italic(); }
        if marks.underline == Some(true) { run = run.underline("single"); }
    }
    let bytes = text.as_bytes();
    let (mut start, mut index) = (0, 0);
    while index < bytes.len() {
        if [b'\t', b'\r', b'\n'].contains(&bytes[index]) {
            if start < index { run = run.add_text(&text[start..index]); }
            if bytes[index] == b'\t' { run = run.add_tab(); }
            else { run = run.add_break(BreakType::TextWrapping); }
            if bytes[index] == b'\r' && bytes.get(index + 1) == Some(&b'\n') { index += 1; }
            index += 1;
            start = index;
        } else { index += 1; }
    }
    if start < text.len() || text.is_empty() { run = run.add_text(&text[start..]); }
    run
}

fn add_runs(mut paragraph: Paragraph, text: &str, marks: &[InlineRun]) -> Paragraph {
    let mut offsets = HashMap::from([(0usize, 0usize)]);
    let mut utf16 = 0;
    for (byte, ch) in text.char_indices() {
        utf16 += ch.len_utf16();
        offsets.insert(utf16, byte + ch.len_utf8());
    }
    let mut cursor = 0;
    for mark in marks {
        let (start, end) = (offsets[&mark.start], offsets[&mark.end]);
        if start > cursor { paragraph = paragraph.add_run(literal_run(&text[cursor..start], None)); }
        paragraph = paragraph.add_run(literal_run(&text[start..end], Some(mark)));
        cursor = end;
    }
    if cursor < text.len() || text.is_empty() { paragraph = paragraph.add_run(literal_run(&text[cursor..], None)); }
    paragraph
}

fn directed_paragraph(mut paragraph: Paragraph, text: &str) -> Paragraph {
    use unicode_bidi::{get_base_direction, Direction};
    paragraph.property = paragraph.property.bidi(get_base_direction(text) == Direction::Rtl);
    paragraph
}

fn plain_paragraph(text: &str) -> Paragraph {
    directed_paragraph(Paragraph::new().line_spacing(LineSpacing::new().before(0).after(0)), text)
        .add_run(literal_run(text, None))
}

#[derive(Default)]
struct Lists { active: Option<(String, usize)>, definitions: Vec<Numbering> }
impl Lists {
    fn paragraph(&mut self, mut paragraph: Paragraph, block: &StructureBlock) -> Paragraph {
        let Some(list) = &block.list else { self.active = None; return paragraph; };
        let same_kind = self.active.as_ref().is_some_and(|(kind, _)| kind == &list.kind);
        if !same_kind || list.start.is_some() {
            let id = DOCX_RS_BUILT_IN_NUMBERING_ID + 1 + self.definitions.len();
            let mut numbering = Numbering::new(id, if list.kind == "bullet" { BULLET_ABSTRACT_ID } else { DECIMAL_ABSTRACT_ID });
            if let Some(start) = list.start { numbering = numbering.add_override(LevelOverride::new(list.level as usize).start(start as usize)); }
            self.definitions.push(numbering);
            self.active = Some((list.kind.clone(), id));
        }
        paragraph = paragraph.numbering(NumberingId::new(self.active.as_ref().unwrap().1), IndentLevel::new(list.level as usize));
        paragraph
    }
}

fn styled_paragraph(block: &StructureBlock, lists: &mut Lists) -> Paragraph {
    let align = match block.alignment.as_str() {
        "center" => AlignmentType::Center, "end" => AlignmentType::End,
        "justify" => AlignmentType::Both, _ => AlignmentType::Start,
    };
    let mut paragraph = Paragraph::new().align(align).line_spacing(LineSpacing::new().before(0).after(0));
    if block.break_before == "page" { paragraph = paragraph.page_break_before(true); }
    if let Some(level) = block.heading_level { paragraph = paragraph.style(&format!("Heading{level}")).keep_next(true); }
    else if block.role == "caption" { paragraph = paragraph.style("Caption"); }
    else if block.role == "footnote" { paragraph = paragraph.style("FootnoteText"); }
    lists.paragraph(paragraph, block)
}

fn compatible_inline(left: &StructureBlock, right: &StructureBlock) -> bool {
    // Separate list-item semantics cannot be collapsed into one numbered item.
    left.role != "list-item" && right.role != "list-item" && left.role == right.role &&
        left.heading_level == right.heading_level && left.alignment == right.alignment &&
        left.section_id == right.section_id && left.column == right.column && right.break_before == "none"
}

fn section_property(page: &DocxPage, columns: u8) -> Result<SectionProperty, String> {
    let width = (page.page_width_points * 20.0).round() as u32;
    let height = (page.page_height_points * 20.0).round() as u32;
    let horizontal = 720.min(width / 10);
    let vertical = 720.min(height / 10);
    let inner = width - 2 * horizontal;
    if inner < u32::from(columns) { return Err(error("the page is too narrow for its selected reading columns")); }
    let mut property = SectionProperty::new()
        .page_size(PageSize::new().size(width, height))
        .page_margin(PageMargin { top: vertical as i32, bottom: vertical as i32, left: horizontal as i32, right: horizontal as i32,
            header: 0, footer: 0, gutter: 0 });
    property.columns = columns as usize;
    property.space = if columns == 2 { (inner / 20).min(360).min(inner - 2) as usize } else { 0 };
    property.section_type = Some(SectionType::Continuous);
    Ok(property)
}

fn content_size(page: &DocxPage, property: &SectionProperty) -> (f64, f64) {
    let width = page.page_width_points - f64::from(property.page_margin.left + property.page_margin.right) / 20.0;
    let height = page.page_height_points - f64::from(property.page_margin.top + property.page_margin.bottom) / 20.0;
    ((width - property.space as f64 / 20.0 * (property.columns - 1) as f64) / property.columns as f64, height)
}

fn table_element(data: &OfficeTableData, available_points: f64) -> Result<Table, String> {
    // Validation has proven a bounded full rectangular grid. A vertical
    // continuation needs a real empty w:tc in every covered row; horizontal
    // covered cells are omitted and represented by gridSpan on that w:tc.
    let mut grid = vec![0usize; data.rows * data.columns];
    for (index, cell) in data.cells.iter().enumerate() {
        for row in cell.row..cell.row + cell.row_span {
            for column in cell.column..cell.column + cell.column_span { grid[row * data.columns + column] = index; }
        }
    }
    let width = (available_points * 20.0).round() as usize;
    if width < data.columns {
        return Err(error("the page or reading column is too narrow for every table column; choose a larger page or a one-column section"));
    }
    let column_widths = (0..data.columns).map(|column| width / data.columns + usize::from(column < width % data.columns)).collect::<Vec<_>>();
    let rows = (0..data.rows).map(|row| {
        let mut cells = Vec::new();
        let mut column = 0;
        while column < data.columns {
            let source = &data.cells[grid[row * data.columns + column]];
            let anchor = source.row == row;
            let mut run = literal_run(if anchor { &source.text } else { "" }, None);
            if source.header == Some(true) && anchor { run = run.bold(); }
            let paragraph = directed_paragraph(Paragraph::new().line_spacing(LineSpacing::new().before(0).after(0)), if anchor { &source.text } else { "" }).add_run(run);
            let mut cell = TableCell::new().add_paragraph(paragraph)
                .width(column_widths[column..column + source.column_span].iter().sum(), WidthType::Dxa);
            if source.column_span > 1 { cell = cell.grid_span(source.column_span); }
            if source.row_span > 1 { cell = cell.vertical_merge(if anchor { VMergeType::Restart } else { VMergeType::Continue }); }
            cells.push(cell);
            column += source.column_span;
        }
        TableRow::new(cells)
    }).collect();
    Ok(Table::new(rows).set_grid(column_widths).width(width, WidthType::Dxa))
}

enum BodyItem { Paragraph(Paragraph), Table(Table) }
struct Part { property: SectionProperty, items: Vec<BodyItem> }

fn prefix_break(items: &mut Vec<BodyItem>, page: bool, column: bool) {
    if !page && !column { return; }
    let paragraph = Paragraph::new().line_spacing(LineSpacing::new().before(0).after(0));
    let paragraph = if page { paragraph.page_break_before(true) } else { paragraph };
    let paragraph = if column { paragraph.add_run(Run::new().add_break(BreakType::Column)) } else { paragraph };
    items.push(BodyItem::Paragraph(paragraph));
}

fn push_object(items: &mut Vec<BodyItem>, object: &OfficeExportObject, available_points: f64, warnings: &mut Vec<String>) -> Result<(), String> {
    match &object.data {
        OfficeObjectData::Table(data) => {
            if data.cells.iter().any(|cell| cell.header == Some(true)) {
                warn(warnings, "Table header cells are bold. Automatic repeated table header rows are not supported by this DOCX writer.");
            }
            items.push(BodyItem::Table(table_element(data, available_points)?));
        },
        OfficeObjectData::Formula(latex) => {
            warn(warnings, "Selected formula objects are exported as literal editable LaTeX; they are not converted into Word equation objects (OMML).");
            items.push(BodyItem::Paragraph(plain_paragraph(latex)));
        },
    }
    Ok(())
}

fn page_parts(page: &DocxPage, first_page: bool, lists: &mut Lists, warnings: &mut Vec<String>) -> Result<Vec<Part>, String> {
    let fallback;
    let structure = if let Some(structure) = &page.structure { structure } else { fallback = default_structure(page); &fallback };
    let regions = page.regions.iter().map(|region| (region.id.as_str(), region)).collect::<HashMap<_, _>>();
    let figures = page.figures.iter().map(|figure| (figure.region_id.as_str(), figure)).collect::<HashMap<_, _>>();
    let replacements = page.objects.iter().filter_map(|object| match &object.placement {
        OfficePlacement::ReplaceRegion { region_id } => Some((region_id.as_str(), object)), _ => None,
    }).collect::<HashMap<_, _>>();
    let blank_section = StructureSection { id: "docx-blank".into(), columns: 1 };
    let sections = if structure.sections.is_empty() { vec![&blank_section] } else { structure.sections.iter().collect::<Vec<_>>() };
    let mut parts = Vec::new();
    let mut text_seen = false;
    for (section_index, section) in sections.iter().enumerate() {
        lists.active = None;
        let property = section_property(page, section.columns)?;
        let (available_width, available_height) = content_size(page, &property);
        let mut items = Vec::new();
        let mut last_inline: Option<StructureBlock> = None;
        let mut previous_column = 1;
        let mut pending_page_break = !first_page && section_index == 0;
        for block in structure.blocks.iter().filter(|block| block.section_id == section.id) {
            let column_break = block.column != previous_column;
            previous_column = block.column;
            if block.role == "figure" {
                lists.active = None;
                last_inline = None;
                let figure = figures[block.source_region_id.as_str()];
                let scale = (available_width / figure.width_points).min(available_height / figure.height_points).min(1.0);
                let width = (figure.width_points * scale * 12_700.0).round().max(1.0) as u32;
                let height = (figure.height_points * scale * 12_700.0).round().max(1.0) as u32;
                let image = Pic::new_with_dimensions(figure.png.clone(), figure.width_px, figure.height_px).size(width, height);
                let mut paragraph = styled_paragraph(block, lists);
                if pending_page_break { paragraph = paragraph.page_break_before(true); }
                if column_break { paragraph = paragraph.add_run(Run::new().add_break(BreakType::Column)); }
                items.push(BodyItem::Paragraph(paragraph.add_run(Run::new().add_image(image))));
                pending_page_break = false;
                if block.alt_text.is_some() || block.decorative.is_some() {
                    warn(warnings, "Figure alternative text and decorative metadata remain in the project/JSON; docx-rs 0.4.20 does not write those accessibility attributes.");
                }
                continue;
            }
            let region = regions[block.source_region_id.as_str()];
            let inline_separator = region.separator_before.as_deref().filter(|separator| ["", " "].contains(separator));
            let replacement = replacements.get(region.id.as_str()).copied();
            if text_seen && inline_separator.is_some() && replacement.is_some_and(|object| matches!(object.data, OfficeObjectData::Table(_))) {
                return Err(error("a table replacement cannot use an empty or space-only passage separator; separate that passage before exporting"));
            }
            if let Some(object) = replacement {
                if matches!(object.data, OfficeObjectData::Table(_)) {
                    if text_seen && region.separator_before.as_deref() == Some("\n\n") { items.push(BodyItem::Paragraph(plain_paragraph(""))); }
                    prefix_break(&mut items, pending_page_break || block.break_before == "page", column_break);
                    push_object(&mut items, object, available_width, warnings)?;
                    pending_page_break = false;
                    last_inline = None;
                    lists.active = None;
                    text_seen = true;
                    continue;
                }
            }
            let text = if let Some(OfficeExportObject { data: OfficeObjectData::Formula(latex), .. }) = replacement {
                warn(warnings, "Selected formula objects are exported as literal editable LaTeX; they are not converted into Word equation objects (OMML).");
                latex.as_str()
            } else { region.text.as_str() };
            if block.role == "table" && replacement.is_none() {
                warn(warnings, "Unreplaced OCR table regions remain literal text. Only explicitly selected extraction table objects become editable Word tables.");
            }
            if block.role == "formula" && replacement.is_none() {
                warn(warnings, "Unreplaced OCR formula regions remain literal text; no equation syntax is inferred.");
            }
            if ["header", "footer"].contains(&block.role.as_str()) {
                warn(warnings, "Page header/footer regions are preserved in body reading order, not converted into repeating Word header/footer parts.");
            }
            if block.role == "footnote" || block.note_references.is_some() {
                warn(warnings, "Footnote text and exact note markers remain in body reading order. Automatic Word footnotes/custom markers are not generated.");
            }
            if block.anchor_block_id.is_some() {
                warn(warnings, "Caption text and source order are preserved; caption-to-figure/table links are not converted into Word cross-reference fields.");
            }
            if block.role == "list-item" {
                warn(warnings, "Lists use Word numbering or bullets. Existing literal bullets/numbers in reviewed text are preserved and may appear beside the generated markers.");
            }
            if text_seen && inline_separator.is_some() {
                if pending_page_break || column_break || !last_inline.as_ref().is_some_and(|previous| compatible_inline(previous, block)) {
                    return Err(error("an empty or space-only passage separator conflicts with a figure, table, list, column, section, or paragraph style boundary; merge or separate those passages before exporting"));
                }
                let Some(BodyItem::Paragraph(previous)) = items.pop() else { return Err(error("inline passage continuation has no preceding text paragraph")); };
                let paragraph = previous.add_run(literal_run(inline_separator.unwrap(), None));
                items.push(BodyItem::Paragraph(add_runs(paragraph, text, &block.runs)));
            } else {
                if text_seen && region.separator_before.as_deref() == Some("\n\n") { items.push(BodyItem::Paragraph(plain_paragraph(""))); }
                let mut paragraph = directed_paragraph(styled_paragraph(block, lists), text);
                if pending_page_break { paragraph = paragraph.page_break_before(true); }
                if column_break { paragraph = paragraph.add_run(Run::new().add_break(BreakType::Column)); }
                items.push(BodyItem::Paragraph(add_runs(paragraph, text, &block.runs)));
            }
            pending_page_break = false;
            text_seen = true;
            last_inline = Some(block.clone());
        }
        if section_index + 1 == sections.len() {
            for object in page.objects.iter().filter(|object| matches!(object.placement, OfficePlacement::Append)) {
                if pending_page_break { prefix_break(&mut items, true, false); pending_page_break = false; }
                lists.active = None;
                push_object(&mut items, object, available_width, warnings)?;
            }
        }
        if items.is_empty() {
            let paragraph = if pending_page_break { plain_paragraph("").page_break_before(true) } else { plain_paragraph("") };
            items.push(BodyItem::Paragraph(paragraph));
        }
        parts.push(Part { property, items });
    }
    Ok(parts)
}

fn styles(mut docx: Docx) -> Docx {
    for level in 1..=6 {
        docx = docx.add_style(Style::new(format!("Heading{level}"), StyleType::Paragraph)
            .name(format!("heading {level}")).based_on("Normal").next("Normal")
            .bold().size([32, 28, 26, 24, 22, 22][level - 1]).outline_lvl(level - 1).q_format(true));
    }
    docx.add_style(Style::new("Caption", StyleType::Paragraph).name("Caption").based_on("Normal").next("Normal").italic().size(20))
        .add_style(Style::new("FootnoteText", StyleType::Paragraph).name("Footnote Text").based_on("Normal").next("Normal").size(20))
}

fn abstract_numbering(id: usize, bullet: bool) -> AbstractNumbering {
    let mut definition = AbstractNumbering::new(id);
    for level in 0..6 {
        definition = definition.add_level(Level::new(level, Start::new(1), NumberFormat::new(if bullet { "bullet" } else { "decimal" }),
            LevelText::new(if bullet { "•".to_owned() } else { format!("%{}.", level + 1) }), LevelJc::new("left"))
            .indent(Some(360 * (level as i32 + 1)), Some(SpecialIndentType::Hanging(180)), None, None));
    }
    definition
}

/// Create actual package bytes, preserving literal reviewed content and the
/// selected source order. Unsupported semantics are disclosed, never guessed.
pub fn render_docx(pages: &[DocxPage]) -> Result<DocxOutput, String> {
    let mut warnings = Vec::new();
    validate_pages(pages, &mut warnings)?;
    let mut lists = Lists::default();
    let mut parts = Vec::new();
    for (index, page) in pages.iter().enumerate() { parts.extend(page_parts(page, index == 0, &mut lists, &mut warnings)?); }
    let part_count = parts.len();
    let mut docx = styles(Docx::new().default_size(22));
    if !lists.definitions.is_empty() {
        docx = docx.add_abstract_numbering(abstract_numbering(BULLET_ABSTRACT_ID, true))
            .add_abstract_numbering(abstract_numbering(DECIMAL_ABSTRACT_ID, false));
        for definition in lists.definitions { docx = docx.add_numbering(definition); }
    }
    for (index, mut part) in parts.into_iter().enumerate() {
        if index + 1 < part_count {
            match part.items.pop() {
                Some(BodyItem::Paragraph(paragraph)) => part.items.push(BodyItem::Paragraph(paragraph.section_property(part.property))),
                Some(table @ BodyItem::Table(_)) => {
                    part.items.push(table);
                    part.items.push(BodyItem::Paragraph(plain_paragraph("").section_property(part.property)));
                },
                None => unreachable!("each prepared section contains at least one block"),
            }
        } else { docx.document.section_property = part.property; }
        for item in part.items {
            docx = match item { BodyItem::Paragraph(paragraph) => docx.add_paragraph(paragraph), BodyItem::Table(table) => docx.add_table(table) };
        }
    }
    let mut output = Cursor::new(Vec::new());
    docx.build().pack(&mut output).map_err(|failure| error(&format!("DOCX package creation failed: {failure}")))?;
    let bytes = output.into_inner();
    if bytes.len() > 128 * 1024 * 1024 { return Err(error("the generated package exceeds 128 MB; no document was written")); }
    Ok(DocxOutput { bytes, warnings })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ocr_document::{NoteReference, StructureList};
    use crate::ocr_office::OfficeTableCell;
    use std::io::Read;

    fn region(id: &str, text: &str) -> DocxRegion {
        DocxRegion { id: id.into(), kind: "text".into(), text: text.into(), separator_before: None }
    }
    fn page(regions: Vec<DocxRegion>) -> DocxPage {
        DocxPage { id: "page".into(), page_width_points: 612.0, page_height_points: 792.0,
            regions, figures: vec![], structure: None, objects: vec![] }
    }
    fn xml(bytes: &[u8], path: &str) -> String {
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        let mut text = String::new();
        archive.by_name(path).unwrap().read_to_string(&mut text).unwrap();
        text
    }
    #[test]
    fn structured_paragraphs_keep_logical_text_and_base_direction() {
        let output = render_docx(&[page(vec![region("rtl", "العربية ABC 123"), region("ltr", "English שלום 456")])]).unwrap();
        let document = xml(&output.bytes, "word/document.xml");
        assert!(document.contains("العربية ABC 123"));
        assert!(document.contains("English שלום 456"));
        let parsed = parse_xml(&document);
        let mut directions = Vec::new();
        parsed.descendants("w:bidi", &mut directions);
        // The writer omits false because left-to-right is the OOXML default.
        assert_eq!(directions.len(), 1);
        assert!(directions[0].attributes.get("w:val").is_none_or(|value| ["1", "true"].contains(&value.as_str())));
    }

    // Inspect the actual packaged XML, including definitions inserted by the
    // library. Presence checks cannot detect a reference resolving to a second
    // definition with the same ID (the original list-start/bullet regression).
    #[derive(Debug)]
    struct XmlElement { name: String, attributes: HashMap<String, String>, children: Vec<XmlElement> }
    impl XmlElement {
        fn attribute(&self, name: &str) -> &str { self.attributes.get(name).unwrap().as_str() }
        fn child(&self, name: &str) -> &XmlElement {
            let mut found = self.children.iter().filter(|child| child.name == name);
            let child = found.next().unwrap();
            assert!(found.next().is_none(), "duplicate {name} in {self:?}");
            child
        }
        fn unique_children(&self, name: &str, key: &str) -> HashMap<&str, &XmlElement> {
            let mut found = HashMap::new();
            for child in self.children.iter().filter(|child| child.name == name) {
                let id = child.attribute(key);
                assert!(found.insert(id, child).is_none(), "duplicate {name} {key}={id}");
            }
            found
        }
        fn descendants<'a>(&'a self, name: &str, found: &mut Vec<&'a XmlElement>) {
            if self.name == name { found.push(self); }
            for child in &self.children { child.descendants(name, found); }
        }
    }
    fn parse_xml(text: &str) -> XmlElement {
        use quick_xml::events::Event;
        let mut reader = quick_xml::Reader::from_str(text);
        let mut stack: Vec<XmlElement> = Vec::new();
        let mut root = None;
        loop {
            let event = reader.read_event().unwrap();
            let empty = matches!(&event, Event::Empty(_));
            match event {
                Event::Start(element) | Event::Empty(element) => {
                    let node = XmlElement {
                        name: String::from_utf8(element.name().as_ref().to_vec()).unwrap(),
                        attributes: element.attributes().map(|attribute| {
                            let attribute = attribute.unwrap();
                            (String::from_utf8(attribute.key.as_ref().to_vec()).unwrap(),
                                attribute.decode_and_unescape_value(reader.decoder()).unwrap().into_owned())
                        }).collect(),
                        children: Vec::new(),
                    };
                    if empty { stack.last_mut().unwrap().children.push(node); }
                    else { stack.push(node); }
                },
                Event::End(_) => {
                    let node = stack.pop().unwrap();
                    if let Some(parent) = stack.last_mut() { parent.children.push(node); }
                    else { assert!(root.replace(node).is_none()); }
                },
                Event::Eof => break,
                _ => {},
            }
        }
        assert!(stack.is_empty());
        root.unwrap()
    }
    fn cell(id: &str, row: usize, column: usize, text: &str) -> OfficeTableCell {
        OfficeTableCell { id: id.into(), row, column, row_span: 1, column_span: 1,
            text: text.into(), header: None, source_box: None }
    }
    fn table_object(data: OfficeTableData) -> OfficeExportObject {
        OfficeExportObject { object_id: "table-object".into(), name: "Reviewed table".into(),
            placement: OfficePlacement::Append, data: OfficeObjectData::Table(data) }
    }
    fn one_cell(text: &str) -> OfficeTableData {
        OfficeTableData { version: 1, rows: 1, columns: 1, cells: vec![cell("cell", 0, 0, text)] }
    }
    fn png() -> Vec<u8> {
        // A real 1x1 RGB PNG with one red pixel (IHDR/IDAT/IEND and valid CRCs).
        vec![137,80,78,71,13,10,26,10,0,0,0,13,73,72,68,82,0,0,0,1,0,0,0,1,8,2,0,0,0,144,119,83,222,
            0,0,0,12,73,68,65,84,120,156,99,248,207,192,0,0,3,1,1,0,201,254,146,239,0,0,0,0,73,69,78,68,174,66,96,130]
    }

    #[test]
    fn literal_spaces_unicode_tabs_and_all_lines_survive_the_package() {
        let output = render_docx(&[page(vec![region("text", "  😀 e\u{301}\tA\r\nB\n\n C  <&>")])]).unwrap();
        let document = xml(&output.bytes, "word/document.xml");
        assert!(document.contains("xml:space=\"preserve\">  😀 e\u{301}</w:t>"), "{document}");
        assert!(document.contains(" C  &lt;&amp;&gt;</w:t>"), "{document}");
        assert_eq!(document.matches("<w:tab").count(), 1);
        assert_eq!(document.matches("w:type=\"textWrapping\"").count(), 3);
        assert!(output.warnings.iter().any(|warning| warning.contains("CRLF")));
    }

    #[test]
    fn exact_empty_and_space_separators_coalesce_without_new_paragraphs() {
        let mut source = page(vec![region("one", "Hel"), region("two", "lo"), region("three", "world")]);
        source.regions[1].separator_before = Some("".into());
        source.regions[2].separator_before = Some(" ".into());
        let document = xml(&render_docx(&[source.clone()]).unwrap().bytes, "word/document.xml");
        assert_eq!(document.matches("<w:p ").count(), 1, "{document}");
        assert!(document.contains(">Hel</w:t>") && document.contains(">lo</w:t>") && document.contains("> </w:t>") && document.contains(">world</w:t>"));
        source.regions.push(region("four", "next"));
        source.regions[3].separator_before = Some("\n\n".into());
        source.regions.push(region("five", ""));
        let document = xml(&render_docx(&[source]).unwrap().bytes, "word/document.xml");
        assert_eq!(document.matches("<w:p ").count(), 4, "blank passage/line must remain: {document}");
    }

    #[test]
    fn incompatible_inline_semantics_fail_instead_of_inserting_text() {
        let mut source = page(vec![region("one", "Hel"), region("two", "lo")]);
        source.regions[1].separator_before = Some("".into());
        let mut structure = default_structure(&source);
        structure.blocks[1].role = "heading".into();
        structure.blocks[1].heading_level = Some(1);
        source.structure = Some(structure);
        assert!(render_docx(&[source]).unwrap_err().contains("passage separator conflicts"));
    }

    #[test]
    fn styled_utf16_ranges_after_emoji_are_real_runs_and_split_surrogates_fail() {
        let mut source = page(vec![region("text", "😀Equipment")]);
        let mut structure = default_structure(&source);
        structure.blocks[0].runs = vec![InlineRun { start: 2, end: 11, bold: Some(true), italic: Some(true), underline: Some(true) }];
        source.structure = Some(structure);
        let document = xml(&render_docx(&[source.clone()]).unwrap().bytes, "word/document.xml");
        assert!(document.contains(">😀</w:t>") && document.contains(">Equipment</w:t>"));
        let styled = document.split(">😀</w:t>").nth(1).unwrap().split(">Equipment</w:t>").next().unwrap();
        assert!(styled.contains("<w:b") && styled.contains("<w:i") && styled.contains("<w:u"), "{styled}");
        source.structure.as_mut().unwrap().blocks[0].runs[0].start = 1;
        assert!(render_docx(&[source]).unwrap_err().contains("complete characters"));
    }

    #[test]
    fn headings_lists_levels_restarts_alignment_and_explicit_break_have_real_definitions() {
        let mut source = page(vec![region("heading", "Heading"), region("list1", "First"), region("list2", "Nested"), region("list3", "Restart")]);
        let mut structure = default_structure(&source);
        structure.blocks[0].role = "heading".into();
        structure.blocks[0].heading_level = Some(2);
        structure.blocks[0].alignment = "center".into();
        for index in 1..4 {
            structure.blocks[index].role = "list-item".into();
            structure.blocks[index].list = Some(StructureList { kind: "number".into(), level: if index == 2 { 1 } else { 0 },
                start: match index { 1 => Some(7), 3 => Some(3), _ => None } });
        }
        structure.blocks[3].break_before = "page".into();
        source.structure = Some(structure);
        let output = render_docx(&[source]).unwrap();
        let document = xml(&output.bytes, "word/document.xml");
        let styles = xml(&output.bytes, "word/styles.xml");
        let numbering = xml(&output.bytes, "word/numbering.xml");
        assert!(document.contains("w:val=\"Heading2\"") && styles.contains("w:styleId=\"Heading2\""));
        assert!(styles.contains("w:outlineLvl w:val=\"1\""));
        assert!(document.contains("w:jc w:val=\"center\""));
        assert_eq!(document.matches("<w:numPr>").count(), 3);
        assert!(document.contains("w:ilvl w:val=\"1\""));
        assert!(numbering.contains("w:startOverride w:val=\"7\"") && numbering.contains("w:startOverride w:val=\"3\""));
        assert!(document.contains("<w:pageBreakBefore"));
        assert!(xml(&output.bytes, "word/_rels/document.xml.rels").contains("numbering.xml"));
        assert!(output.warnings.iter().any(|warning| warning.contains("Existing literal bullets/numbers")));
    }

    #[test]
    fn list_references_resolve_to_unique_numbering_and_effective_requested_levels() {
        let mut source = page(vec![region("first", "7. Retain literal marker"), region("nested", "Nested number"),
            region("restart", "Restart"), region("bullet", "Nested bullet"), region("bullet2", "Next bullet")]);
        let mut structure = default_structure(&source);
        for (index, (kind, level, start)) in [
            ("number", 0, Some(7)), ("number", 1, None), ("number", 0, Some(3)),
            ("bullet", 1, None), ("bullet", 0, None),
        ].into_iter().enumerate() {
            structure.blocks[index].role = "list-item".into();
            structure.blocks[index].list = Some(StructureList { kind: kind.into(), level, start });
        }
        source.structure = Some(structure);
        let output = render_docx(&[source]).unwrap();
        let document = parse_xml(&xml(&output.bytes, "word/document.xml"));
        let numbering = parse_xml(&xml(&output.bytes, "word/numbering.xml"));
        let abstracts = numbering.unique_children("w:abstractNum", "w:abstractNumId");
        let numbers = numbering.unique_children("w:num", "w:numId");
        // Pinned docx-rs inserts these without exposing them in the builder's
        // vectors. The emitted package, rather than those vectors, is decisive.
        assert!(abstracts.contains_key("1") && numbers.contains_key("1"));
        for definition in abstracts.values() { definition.unique_children("w:lvl", "w:ilvl"); }
        for definition in numbers.values() { definition.unique_children("w:lvlOverride", "w:ilvl"); }

        let mut paragraphs = Vec::new();
        document.descendants("w:numPr", &mut paragraphs);
        assert_eq!(paragraphs.len(), 5);
        let expected = [("decimal", "%1.", 7), ("decimal", "%2.", 1), ("decimal", "%1.", 3),
            ("bullet", "•", 1), ("bullet", "•", 1)];
        let mut referenced_ids = Vec::new();
        for (properties, (format, marker, start)) in paragraphs.into_iter().zip(expected) {
            let id = properties.child("w:numId").attribute("w:val");
            let level_id = properties.child("w:ilvl").attribute("w:val");
            assert_ne!(id, "1", "reviewed lists must not reference the library's built-in list");
            referenced_ids.push(id.to_owned());
            let number = numbers[id];
            let abstract_id = number.child("w:abstractNumId").attribute("w:val");
            let definition = abstracts[abstract_id];
            let levels = definition.unique_children("w:lvl", "w:ilvl");
            let overrides = number.unique_children("w:lvlOverride", "w:ilvl");
            let level_override = overrides.get(level_id).copied();
            let level = level_override.and_then(|value| value.children.iter().find(|child| child.name == "w:lvl"))
                .unwrap_or(levels[level_id]);
            let effective_start = level_override.and_then(|value| value.children.iter().find(|child| child.name == "w:startOverride"))
                .unwrap_or_else(|| level.child("w:start")).attribute("w:val").parse::<u32>().unwrap();
            assert_eq!(level.child("w:numFmt").attribute("w:val"), format);
            assert_eq!(level.child("w:lvlText").attribute("w:val"), marker);
            assert_eq!(effective_start, start);
        }
        assert_eq!(referenced_ids[0], referenced_ids[1], "a nested decimal continues its list instance");
        assert_ne!(referenced_ids[0], referenced_ids[2], "an explicit restart has a separate instance");
        assert_ne!(referenced_ids[2], referenced_ids[3], "changing to bullets has a separate instance");
        assert_eq!(referenced_ids[3], referenced_ids[4]);
    }

    #[test]
    fn two_dimensional_spans_emit_continuation_cells_without_repeated_text_or_numeric_inference() {
        let mut header = cell("span", 0, 0, "0012\n=SUM(A1)");
        header.row_span = 2; header.column_span = 2; header.header = Some(true);
        let data = OfficeTableData { version: 1, rows: 3, columns: 3, cells: vec![header,
            cell("currency", 0, 2, "€1,250.00"), cell("empty", 1, 2, ""),
            cell("at", 2, 0, "@literal"), cell("plus", 2, 1, "+12"), cell("minus", 2, 2, "-0")] };
        let mut source = page(vec![]); source.objects.push(table_object(data));
        let output = render_docx(&[source]).unwrap();
        let document = xml(&output.bytes, "word/document.xml");
        assert_eq!(document.matches("w:gridSpan w:val=\"2\"").count(), 2);
        assert_eq!(document.matches("w:vMerge w:val=\"restart\"").count(), 1);
        assert_eq!(document.matches("w:vMerge w:val=\"continue\"").count(), 1);
        assert_eq!(document.matches(">0012</w:t>").count(), 1);
        assert!(document.contains("<w:rPr><w:b"), "header bold belongs to actual text run: {document}");
        for literal in ["=SUM(A1)", "€1,250.00", "@literal", "+12", "-0"] { assert!(document.contains(literal)); }
        assert!(!document.contains("<w:instrText"));
        assert!(output.warnings.iter().any(|warning| warning.contains("repeated")));
    }

    #[test]
    fn only_explicit_same_kind_replacement_removes_the_old_literal_projection() {
        let mut source = page(vec![region("table-region", "OLD SOURCE")]);
        source.regions[0].kind = "table".into();
        source.objects.push(table_object(one_cell("NEW CURRENT VALUE")));
        let appended = xml(&render_docx(&[source.clone()]).unwrap().bytes, "word/document.xml");
        assert!(appended.contains("OLD SOURCE") && appended.contains("NEW CURRENT VALUE"));
        source.objects[0].placement = OfficePlacement::ReplaceRegion { region_id: "table-region".into() };
        let replaced = xml(&render_docx(&[source.clone()]).unwrap().bytes, "word/document.xml");
        assert!(!replaced.contains("OLD SOURCE") && replaced.contains("NEW CURRENT VALUE"));
        source.regions[0].kind = "text".into();
        assert!(render_docx(&[source]).unwrap_err().contains("same kind"));
    }

    #[test]
    fn figures_columns_literal_notes_and_blank_source_pages_are_retained_with_disclosed_limits() {
        let mut source = page(vec![region("body", "Before [x]"), region("caption", "Literal caption"), region("note", "Note [x]")]);
        source.figures.push(DocxFigure { region_id: "figure".into(), png: png(), width_px: 1, height_px: 1, width_points: 100.0, height_points: 100.0 });
        let mut structure = default_structure(&source);
        structure.sections[0].columns = 2;
        structure.blocks[0].note_references = Some(vec![NoteReference { start: 7, end: 10, target_block_id: "docx-block-2".into() }]);
        structure.blocks[1].role = "caption".into();
        structure.blocks[1].anchor_block_id = Some("figure-block".into());
        structure.blocks[1].column = 2;
        structure.blocks[2].role = "footnote".into();
        structure.blocks[2].column = 2;
        let mut figure_block = structure.blocks[1].clone();
        figure_block.id = "figure-block".into(); figure_block.source_region_id = "figure".into(); figure_block.role = "figure".into();
        figure_block.anchor_block_id = None; figure_block.alt_text = Some("Red sample".into()); figure_block.decorative = Some(false);
        structure.blocks.insert(1, figure_block);
        source.structure = Some(structure);
        let mut blank = page(vec![]); blank.id = "blank".into();
        let mut last = page(vec![region("last", "Last source page")]); last.id = "last-page".into();
        let output = render_docx(&[source, blank, last]).unwrap();
        let document = xml(&output.bytes, "word/document.xml");
        assert!(document.contains("w:num=\"2\"") && document.contains("w:type=\"column\""));
        assert_eq!(document.matches("<w:pageBreakBefore").count(), 2, "each later source page starts once: {document}");
        for text in ["Before [x]", "Literal caption", "Note [x]", "Last source page"] { assert!(document.contains(text)); }
        let mut archive = zip::ZipArchive::new(Cursor::new(&output.bytes)).unwrap();
        let mut images = Vec::new();
        for index in 0..archive.len() {
            let mut file = archive.by_index(index).unwrap();
            if !file.is_dir() && file.name().starts_with("word/media/") { let mut bytes = Vec::new(); file.read_to_end(&mut bytes).unwrap(); images.push(bytes); }
        }
        assert_eq!(images, vec![png()]);
        assert!(output.warnings.iter().any(|warning| warning.contains("alternative text")));
        assert!(output.warnings.iter().any(|warning| warning.contains("note markers")));
        assert!(output.warnings.iter().any(|warning| warning.contains("cross-reference")));
    }

    #[test]
    fn formulas_and_headers_remain_literal_body_text() {
        let mut source = page(vec![region("head", "Header"), region("foot", "Footer")]);
        source.regions[0].kind = "header".into(); source.regions[1].kind = "footer".into();
        source.objects.push(OfficeExportObject { object_id: "formula".into(), name: "Formula".into(), placement: OfficePlacement::Append,
            data: OfficeObjectData::Formula("\\frac{a}{b} = +0012\n  literal".into()) });
        let output = render_docx(&[source]).unwrap();
        let document = xml(&output.bytes, "word/document.xml");
        assert!(document.contains("Header") && document.contains("Footer") && document.contains("\\frac{a}{b} = +0012"));
        assert!(!document.contains("<m:oMath"));
        assert!(output.warnings.iter().any(|warning| warning.contains("header/footer")));
        assert!(output.warnings.iter().any(|warning| warning.contains("LaTeX")));
    }

    #[test]
    fn invalid_native_inputs_and_word_specific_size_limits_fail_before_output() {
        let mut source = page(vec![region("text", "Text")]);
        source.page_width_points = 1584.0;
        assert!(render_docx(&[source.clone()]).is_ok());
        source.page_width_points = 1584.01;
        assert!(render_docx(&[source]).unwrap_err().contains("1584"));
        let mut source = page(vec![]);
        source.objects.push(table_object(OfficeTableData { version: 1, rows: 1, columns: 64,
            cells: (0..64).map(|column| cell(&format!("c{column}"), 0, column, "")).collect() }));
        assert!(render_docx(&[source.clone()]).unwrap_err().contains("63"));
        source.objects[0] = table_object(OfficeTableData { version: 1, rows: 1, columns: 63,
            cells: (0..63).map(|column| cell(&format!("c{column}"), 0, column, "")).collect() });
        source.page_width_points = 1.0;
        assert!(render_docx(&[source]).unwrap_err().contains("too narrow"));
        assert!(render_docx(&[page(vec![region("invalid", "bad\u{0001}text")])]).is_err());
        assert!(render_docx(&[page(vec![region("same", "a"), region("same", "b")])]).is_err());
        let mut source = page(vec![]);
        source.objects.push(table_object(OfficeTableData { version: 1, rows: 0, columns: 0, cells: vec![] }));
        assert!(render_docx(&[source]).unwrap_err().contains("0 by 0"));
    }
}
