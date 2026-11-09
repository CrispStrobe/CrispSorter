//! Deterministic inputs for the actual production Office validator and writer.
//! This module does not implement an exporter. The Python harness imports the
//! application modules, runs their tests, then validates these generated files
//! independently with ZIP and XML readers.
use crate::ocr_document::{DocumentStructure, InlineRun, NoteReference, StructureBlock, StructureList, StructureSection};
use crate::ocr_docx::{render_docx, DocxFigure, DocxPage, DocxRegion};
use crate::ocr_office::{select_office_objects, OfficeObjectData, OfficeObjectSelection, OfficeOptions, OfficePlacement, OfficeSourcePage, OfficeSourceRegion, OfficeTableCell, OfficeTableData};
use crate::ocr_office::{select_workbook_tables, WorkbookOptions, WorkbookSheetSelection};
use crate::ocr_xlsx::{render_xlsx, XlsxSheet};
use serde_json::{json, Value};
use std::fs;
use std::io::Cursor;
use std::path::Path;

fn region(id: &str, kind: &str, text: &str) -> DocxRegion {
    DocxRegion { id: id.into(), kind: kind.into(), text: text.into(), separator_before: None }
}
fn page(id: &str, regions: Vec<DocxRegion>) -> DocxPage {
    DocxPage { id: id.into(), page_width_points: 612.0, page_height_points: 792.0,
        regions, figures: vec![], structure: None, objects: vec![] }
}
fn block(id: &str, source: &str, section: &str, column: u8, role: &str) -> StructureBlock {
    StructureBlock { id: id.into(), source_region_id: source.into(), section_id: section.into(), column, role: role.into(),
        heading_level: None, list: None, alignment: "start".into(), break_before: "none".into(), runs: vec![],
        anchor_block_id: None, note_references: None, alt_text: None, decorative: None }
}
fn utf16_range(text: &str, needle: &str) -> (usize, usize) {
    let start = text.find(needle).expect("authored fixture substring exists");
    let offset = text[..start].encode_utf16().count();
    (offset, offset + needle.encode_utf16().count())
}
fn mark(text: &str, needle: &str, bold: bool, italic: bool, underline: bool) -> InlineRun {
    let (start, end) = utf16_range(text, needle);
    InlineRun { start, end, bold: bold.then_some(true), italic: italic.then_some(true), underline: underline.then_some(true) }
}
fn cell(id: &str, row: usize, column: usize, row_span: usize, column_span: usize, text: &str, header: bool) -> OfficeTableCell {
    OfficeTableCell { id: id.into(), row, column, row_span, column_span, text: text.into(), header: header.then_some(true), source_box: None }
}

fn semantic_fixture() -> (Vec<DocxPage>, Value) {
    let body = "😀 Equipment & safety\nKeep 0012\tintact. See [1].";
    let texts = ["Maintenance checklist", body, "7. Retain literal marker", "Nested item", "Right column paragraph", "Source header 0012", "[1] Keep the original note.", "Source footer"];
    let mut result = page("semantic-page", vec![
        region("heading", "title", texts[0]), region("body", "text", texts[1]),
        region("number", "text", texts[2]), region("bullet", "text", texts[3]),
        region("right", "text", texts[4]), region("header", "header", texts[5]),
        region("note", "text", texts[6]), region("footer", "footer", texts[7]),
    ]);
    let mut blocks = vec![
        block("b-heading", "heading", "columns", 1, "heading"), block("b-body", "body", "columns", 1, "paragraph"),
        block("b-number", "number", "columns", 1, "list-item"), block("b-bullet", "bullet", "columns", 1, "list-item"),
        block("b-right", "right", "columns", 2, "paragraph"), block("b-header", "header", "notes", 1, "header"),
        block("b-note", "note", "notes", 1, "footnote"), block("b-footer", "footer", "notes", 1, "footer"),
    ];
    blocks[0].heading_level = Some(2);
    blocks[0].alignment = "center".into();
    blocks[1].runs = vec![mark(body, "Equipment", true, false, false), mark(body, "safety", false, true, false), mark(body, "0012", false, false, true)];
    let (start, end) = utf16_range(body, "[1]");
    blocks[1].note_references = Some(vec![NoteReference { start, end, target_block_id: "b-note".into() }]);
    blocks[2].list = Some(StructureList { kind: "number".into(), level: 0, start: Some(7) });
    blocks[3].list = Some(StructureList { kind: "bullet".into(), level: 1, start: None });
    blocks[7].break_before = "page".into();
    result.structure = Some(DocumentStructure { version: 1, sections: vec![StructureSection { id: "columns".into(), columns: 2 }, StructureSection { id: "notes".into(), columns: 1 }], blocks });
    (vec![result], json!({
        "body_paragraphs": texts, "table_grids": [], "images": [], "column_counts": [2,1],
        "minimum_page_breaks": 1, "column_breaks": 1,
        "paragraph_styles": [{"text":texts[0],"style":"Heading2"},{"text":texts[6],"style":"FootnoteText"}],
        "marks": [{"paragraph":body,"text":"Equipment","bold":true},{"paragraph":body,"text":"safety","italic":true},{"paragraph":body,"text":"0012","underline":true}],
        "plain_fragments": [{"paragraph":body,"text":"😀 "},{"paragraph":body,"text":" & "}],
        "lists": [{"text":texts[2],"format":"decimal","level":0,"start":7},{"text":texts[3],"format":"bullet","level":1}],
        "warning_substrings": ["note", "header/footer", "literal bullets/numbers"],
        "forbidden_text": [], "native_footnotes": false
    }))
}

fn whitespace_fixture() -> (Vec<DocxPage>, Value) {
    let mut first = page("whitespace-page", vec![region("a", "text", "  A "), region("b", "text", "😀 B\tC\r\nD"), region("c", "text", "  ")]);
    first.regions[1].separator_before = Some(String::new());
    first.regions[2].separator_before = Some(" ".into());
    let blank = page("blank-page", vec![]);
    let last = page("last-page", vec![region("last", "text", "After blank source page")]);
    (vec![first, blank, last], json!({
        "body_paragraphs": ["  A 😀 B\tC\nD   ", "After blank source page"], "table_grids": [], "images": [],
        "column_counts": [1,1,1], "minimum_page_breaks": 2, "column_breaks": 0,
        "paragraph_styles": [], "marks": [], "plain_fragments": [], "lists": [],
        "warning_substrings": ["CRLF"], "forbidden_text": [], "native_footnotes": false
    }))
}

fn selected_objects_fixture(png: &[u8]) -> Result<(Vec<DocxPage>, Value, Value), String> {
    let table = OfficeTableData { version: 1, rows: 3, columns: 3, cells: vec![
        cell("merged", 0, 0, 2, 2, "Valve\n0012", true), cell("quantity", 0, 2, 1, 1, "Qty", true),
        cell("literal-formula", 1, 2, 1, 1, "=2+2", false), cell("leading-zero", 2, 0, 1, 1, "03", false),
        cell("identifier", 2, 1, 1, 1, "LOT-003", false), cell("xml-literal", 2, 2, 1, 1, " & < > ", false),
    ] };
    let mut original = table.clone(); original.cells[0].text = "ORIGINAL_MODEL_CELL_MUST_NOT_EXPORT".into();
    let capture = json!({"id":"capture","pageId":"objects-page","documentId":"fixture-document","width":800,"height":1000,
        "imagePath":"/never/open/a/provenance/path.png","regionId":"table-region","bounds":{"x":10,"y":20,"w":500,"h":200},"method":"pixels","text":null});
    let make_object = |id: &str, kind: &str, excluded: bool, baseline: Value, current: Value, raw: Value| json!({
        "version":1,"id":id,"kind":kind,"name":id,"createdAt":"2026-11-09T00:00:00.000Z","updatedAt":"2026-11-09T00:00:00.000Z",
        "excluded":excluded,"review":"corrected","captures":[capture.clone()],"originalOutput":raw,"originalOutputKind":"model","originalData":baseline,"currentData":current
    });
    let latex = "\\frac{0012}{03} + x_{LOT-003}";
    let mut unselected = table.clone(); unselected.cells[0].text = "UNSELECTED_TABLE_MUST_NOT_EXPORT".into();
    let provenance = json!({"extractionObjects":{"version":1,"objects":[
        make_object("reviewed-table", "table", false, json!(original), json!(table), json!({"html":"RAW_MODEL_OUTPUT_MUST_NOT_EXPORT"})),
        make_object("reviewed-formula", "formula", false, json!({"version":1,"latex":"ORIGINAL_FORMULA_MUST_NOT_EXPORT"}), json!({"version":1,"latex":latex}), json!("RAW_FORMULA_MUST_NOT_EXPORT")),
        make_object("unselected-table", "table", false, json!(unselected), json!(unselected), json!({"html":"UNSELECTED_RAW"})),
        make_object("excluded-table", "table", true, json!(table), json!(table), json!({"html":"EXCLUDED_RAW"}))
    ]}});
    let source_pages = [OfficeSourcePage { id: "objects-page", provenance: Some(&provenance), regions: vec![OfficeSourceRegion { id: "table-region", kind: "table" }] }];
    let options = OfficeOptions { version: 1, objects: vec![
        OfficeObjectSelection { page_id: "objects-page".into(), object_id: "reviewed-table".into(), placement: "replace-region".into(), region_id: Some("table-region".into()) },
        OfficeObjectSelection { page_id: "objects-page".into(), object_id: "reviewed-formula".into(), placement: "append".into(), region_id: None },
    ] };
    let objects = select_office_objects(&source_pages, &options)?;
    if objects.iter().any(|(page_id, _)| page_id != "objects-page") { return Err("selected object escaped its source page".into()); }
    let mut result = page("objects-page", vec![region("intro", "text", "Reviewed selected objects"), region("table-region", "table", "OCR_TABLE_TEXT_MUST_BE_REPLACED"), region("caption", "text", "Figure 1. Captured color test")]);
    result.objects = objects.into_iter().map(|(_, object)| object).collect();
    result.figures.push(DocxFigure { region_id: "figure".into(), png: png.to_vec(), width_px: 8, height_px: 4, width_points: 160.0, height_points: 80.0 });
    let mut blocks = vec![block("b-intro", "intro", "section", 1, "paragraph"), block("b-table", "table-region", "section", 1, "table"), block("b-figure", "figure", "section", 1, "figure"), block("b-caption", "caption", "section", 1, "caption")];
    blocks[2].alt_text = Some("An authored red and blue test image".into());
    blocks[2].decorative = Some(false);
    blocks[3].anchor_block_id = Some("b-figure".into());
    result.structure = Some(DocumentStructure { version: 1, sections: vec![StructureSection { id: "section".into(), columns: 1 }], blocks });
    let expected = json!({
        "body_paragraphs":["Reviewed selected objects","Figure 1. Captured color test",latex],
        "table_grids":[{"rows":3,"columns":3,"cells":table.cells}], "images":[{"width":8,"height":4}],
        "column_counts":[1],"minimum_page_breaks":0,"column_breaks":0,
        "paragraph_styles":[{"text":"Figure 1. Captured color test","style":"Caption"}],"marks":[],"plain_fragments":[],"lists":[],
        "warning_substrings":["repeated","LaTeX","alternative text","cross-reference"],
        "forbidden_text":["ORIGINAL_MODEL_CELL_MUST_NOT_EXPORT","UNSELECTED_TABLE_MUST_NOT_EXPORT","RAW_MODEL_OUTPUT_MUST_NOT_EXPORT","ORIGINAL_FORMULA_MUST_NOT_EXPORT","RAW_FORMULA_MUST_NOT_EXPORT","OCR_TABLE_TEXT_MUST_BE_REPLACED"],
        "native_footnotes":false
    });
    Ok((vec![result], expected, json!({"provenance":provenance,"options":options})))
}

fn source_snapshot(pages: &[DocxPage]) -> Value {
    json!(pages.iter().map(|page| json!({
        "id":page.id,"pageWidthPoints":page.page_width_points,"pageHeightPoints":page.page_height_points,
        "regions":page.regions.iter().map(|region| json!({"id":region.id,"kind":region.kind,"text":region.text,"separatorBefore":region.separator_before})).collect::<Vec<_>>(),
        "structure":page.structure,"figures":page.figures.iter().map(|figure| json!({"regionId":figure.region_id,"pngInput":"fixture-source.png","widthPx":figure.width_px,"heightPx":figure.height_px,"widthPoints":figure.width_points,"heightPoints":figure.height_points})).collect::<Vec<_>>(),
        "objects":page.objects.iter().map(|object| json!({"objectId":object.object_id,"name":object.name,"placement":match &object.placement {OfficePlacement::Append => json!({"kind":"append"}),OfficePlacement::ReplaceRegion{region_id}=>json!({"kind":"replace-region","regionId":region_id})},"data":match &object.data{OfficeObjectData::Table(data)=>json!({"kind":"table","value":data}),OfficeObjectData::Formula(text)=>json!({"kind":"formula","value":text})}})).collect::<Vec<_>>()
    })).collect::<Vec<_>>())
}
fn write_json(path: &Path, value: &Value) -> Result<(), String> {
    fs::write(path, serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?).map_err(|error| error.to_string())
}

fn rectangular_table(rows: &[Vec<&str>], headers: bool) -> OfficeTableData {
    OfficeTableData { version: 1, rows: rows.len(), columns: rows.first().map_or(0, Vec::len),
        cells: rows.iter().enumerate().flat_map(|(row, values)| values.iter().enumerate().map(move |(column, text)|
            cell(&format!("cell-{row}-{column}"), row, column, 1, 1, text, headers && row == 0))).collect() }
}

fn workbook_provenance(page_id: &str, object_id: &str, table: &OfficeTableData) -> Value {
    let mut original = table.clone();
    if let Some(first) = original.cells.first_mut() { first.text = "ORIGINAL_WORKBOOK_CELL_MUST_NOT_EXPORT".into(); }
    let make = |id: &str, data: &OfficeTableData, excluded: bool| json!({
        "version":1,"id":id,"kind":"table","name":id,
        "createdAt":"2026-11-09T00:00:00.000Z","updatedAt":"2026-11-09T00:00:00.000Z",
        "excluded":excluded,"review":"corrected",
        "captures":[{"id":"capture","pageId":page_id,"documentId":"workbook-document","width":1000,"height":1000,
            "imagePath":"/unavailable/historical-source.png","regionId":null,"bounds":null,"method":"pixels","text":null}],
        "originalOutput":{"html":"RAW_WORKBOOK_OUTPUT_MUST_NOT_EXPORT"},"originalOutputKind":"model",
        "originalData":original,"currentData":data
    });
    let mut unused = table.clone();
    if let Some(first) = unused.cells.first_mut() { first.text = "UNSELECTED_WORKBOOK_CELL_MUST_NOT_EXPORT".into(); }
    json!({"extractionObjects":{"version":1,"objects":[make(object_id, table, false), make("unselected-table", &unused, false), make("excluded-table", &unused, true)]}})
}

fn generate_workbooks(output: &Path) -> Result<(), String> {
    let merged = OfficeTableData { version: 1, rows: 3, columns: 3, cells: vec![
        cell("merged", 0, 0, 2, 2, "Valve\n0012", true), cell("quantity", 0, 2, 1, 1, "Qty", true),
        cell("formula-text", 1, 2, 1, 1, "=2+2", false), cell("code", 2, 0, 1, 1, "03", false),
        cell("identifier", 2, 1, 1, 1, "LOT-003", false), cell("empty", 2, 2, 1, 1, "", false),
    ] };
    let literals = rectangular_table(&[
        vec!["Code", "Literal formulas", "Notes", "Blank"],
        vec!["0012", "=2+2", "😀\tline\nnext", ""],
        vec!["03-04", "+SUM(A1:A2)", "  A & <B>  ", "@value"],
        vec!["-12.5", "1,250.00", "TRUE", "3.14e5"],
    ], true);
    let wide = OfficeTableData { version: 1, rows: 1, columns: 64,
        cells: (0..64).map(|column| cell(&format!("wide-{column}"), 0, column, 1, 1, &format!("ID{:04}", column + 1), false)).collect() };
    let escapes = rectangular_table(&[
        vec!["Actual CR", "left\rright"], vec!["Actual CRLF", "a\r\nb"],
        vec!["Literal escape", "_x000D_"], vec!["Literal underscore escape", "_x005F_"],
        vec!["Adjacent literals", "_x000D__x005F_"], vec!["Mixed", "real\r literal _x000D_ and 😀"],
        vec!["LF and tab", "\n\t"], vec!["Empty string", ""],
    ], false);
    let blank = OfficeTableData { version: 1, rows: 0, columns: 0, cells: vec![] };
    let cases = vec![
        ("reviewed-workbook", vec![("merged-page", "merged-object", "Parts & QA", merged), ("literal-page", "literal-object", "測定 values", literals), ("wide-page", "wide-object", "Wide identifiers", wide)], vec![1usize, 0, 2], Vec::<&str>::new()),
        ("escaped-and-blank-workbook", vec![("escape-page", "escape-object", "Literal _x000D_", escapes), ("empty-page", "empty-object", "Blank table", blank)], vec![0usize, 1], vec!["empty"]),
    ];
    let mut fixtures = Vec::new();
    let mut inputs = Vec::new();
    for (name, tables, order, warning_substrings) in cases {
        let provenance = tables.iter().map(|(page_id, object_id, _, table)| workbook_provenance(page_id, object_id, table)).collect::<Vec<_>>();
        // A saved workbook uses table strings, even when captured image files
        // and current OCR passages are unavailable. No provenance path is read.
        let sources = tables.iter().zip(&provenance).map(|((page_id, _, _, _), value)|
            OfficeSourcePage { id: page_id, provenance: Some(value), regions: vec![] }).collect::<Vec<_>>();
        let options = WorkbookOptions { version: 1, sheets: order.iter().map(|index| {
            let (page_id, object_id, sheet_name, _) = &tables[*index];
            WorkbookSheetSelection { page_id: (*page_id).into(), object_id: (*object_id).into(), sheet_name: (*sheet_name).into() }
        }).collect() };
        let sheets = select_workbook_tables(&sources, &options)?.into_iter().map(|sheet|
            XlsxSheet { page_id: sheet.page_id, object_id: sheet.object_id, name: sheet.name, table: sheet.table }).collect::<Vec<_>>();
        let expected_sheets = order.iter().map(|index| {
            let (page_id, object_id, sheet_name, table) = &tables[*index];
            json!({"name":sheet_name,"pageId":page_id,"objectId":object_id,"table":table})
        }).collect::<Vec<_>>();
        let workbook = render_xlsx(&sheets)?;
        let file = format!("{name}.xlsx");
        fs::write(output.join(&file), workbook.bytes).map_err(|error| error.to_string())?;
        inputs.push(json!({"name":name,"sourcePages":tables.iter().zip(&provenance).map(|((page_id, _, _, _), value)| json!({"id":page_id,"regions":[],"provenance":value})).collect::<Vec<_>>(),"options":options}));
        fixtures.push(json!({"name":name,"file":file,"warnings":workbook.warnings,"expected":{"sheets":expected_sheets,
            "warning_substrings":warning_substrings,"forbidden_text":["ORIGINAL_WORKBOOK_CELL_MUST_NOT_EXPORT","UNSELECTED_WORKBOOK_CELL_MUST_NOT_EXPORT","RAW_WORKBOOK_OUTPUT_MUST_NOT_EXPORT"]}}));
    }
    write_json(&output.join("workbook-inputs.json"), &json!({"version":1,"fixtures":inputs}))?;
    write_json(&output.join("workbooks.json"), &json!({"version":1,"fixtures":fixtures}))
}

pub fn generate(output: &Path) -> Result<(), String> {
    fs::create_dir_all(output).map_err(|error| error.to_string())?;
    let image = image::RgbImage::from_fn(8, 4, |x, _| if x < 4 { image::Rgb([220, 40, 40]) } else { image::Rgb([40, 80, 220]) });
    let mut png = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(image).write_to(&mut png, image::ImageFormat::Png).map_err(|error| error.to_string())?;
    let png = png.into_inner();
    fs::write(output.join("fixture-source.png"), &png).map_err(|error| error.to_string())?;
    let (semantic, semantic_expected) = semantic_fixture();
    let (whitespace, whitespace_expected) = whitespace_fixture();
    let (selected, selected_expected, selection_input) = selected_objects_fixture(&png)?;
    let cases = [("semantic-text", semantic, semantic_expected), ("whitespace-blank-pages", whitespace, whitespace_expected), ("selected-table-figure-formula", selected, selected_expected)];
    let mut manifest = Vec::new();
    let mut inputs = Vec::new();
    for (name, pages, expected) in cases {
        let output_docx = render_docx(&pages)?;
        let file = format!("{name}.docx");
        fs::write(output.join(&file), &output_docx.bytes).map_err(|error| error.to_string())?;
        inputs.push(json!({"name":name,"pages":source_snapshot(&pages)}));
        manifest.push(json!({"name":name,"file":file,"expected":expected,"warnings":output_docx.warnings}));
    }
    write_json(&output.join("inputs.json"), &json!({"version":1,"fixtures":inputs,"selectedObjectBoundary":selection_input}))?;
    write_json(&output.join("fixtures.json"), &json!({"version":1,"fixtures":manifest}))?;
    generate_workbooks(output)?;
    Ok(())
}
