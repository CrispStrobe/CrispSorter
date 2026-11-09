//! Pure, bounded input validation for OCR Office exports.
//!
//! Provenance is untrusted JSON at the native boundary. No path in a capture is
//! opened here: only current, explicitly selected cell/formula values leave this
//! module. The workspace adapter resolves current picture pixels separately.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

const MAX_COLLECTION_BYTES: usize = 16 * 1024 * 1024;
const MAX_RAW_BYTES: usize = 4 * 1024 * 1024;
const MAX_TEXT: usize = 1_000_000;
pub const MAX_OFFICE_OBJECTS: usize = 512;
pub const MAX_WORKBOOK_GRID_POSITIONS: usize = 1_000_000;
pub const MAX_WORKBOOK_TEXT_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OfficeBounds { pub x: f64, pub y: f64, pub w: f64, pub h: f64 }
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OfficeCellBox { pub capture_id: String, pub bounds: OfficeBounds }
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OfficeTableCell {
    pub id: String, pub row: usize, pub column: usize,
    pub row_span: usize, pub column_span: usize, pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<bool>,
    #[serde(rename = "box", default, skip_serializing_if = "Option::is_none")]
    pub source_box: Option<OfficeCellBox>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OfficeTableData {
    pub version: u32, pub rows: usize, pub columns: usize, pub cells: Vec<OfficeTableCell>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OfficeCapture {
    pub id: String, pub page_id: String, pub document_id: String,
    pub width: u32, pub height: u32, pub image_path: String,
    pub region_id: Option<String>, pub bounds: Option<OfficeBounds>,
    pub method: String, pub text: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OfficeObjectSelection {
    pub page_id: String, pub object_id: String, pub placement: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region_id: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OfficeOptions { pub version: u32, pub objects: Vec<OfficeObjectSelection> }
impl Default for OfficeOptions {
    fn default() -> Self { Self { version: 1, objects: vec![] } }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkbookSheetSelection {
    pub page_id: String, pub object_id: String, pub sheet_name: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkbookOptions { pub version: u32, pub sheets: Vec<WorkbookSheetSelection> }
impl Default for WorkbookOptions {
    fn default() -> Self { Self { version: 1, sheets: vec![] } }
}
/// Resolved table strings only. No current or historical image is needed for a
/// workbook, and tab order is the explicit selection order.
#[derive(Clone, Debug, PartialEq)]
pub struct OfficeWorkbookSheet {
    pub page_id: String, pub object_id: String, pub name: String, pub table: OfficeTableData,
}
#[derive(Clone, Debug, PartialEq)]
pub enum OfficePlacement { Append, ReplaceRegion { region_id: String } }
#[derive(Clone, Debug, PartialEq)]
pub enum OfficeObjectData { Table(OfficeTableData), Formula(String) }
#[derive(Clone, Debug, PartialEq)]
pub struct OfficeExportObject {
    pub object_id: String, pub name: String,
    pub placement: OfficePlacement, pub data: OfficeObjectData,
}
#[derive(Clone, Debug)]
pub struct ValidatedExtractionObject {
    pub id: String, pub name: String, pub kind: String, pub excluded: bool,
    pub captures: Vec<OfficeCapture>, pub original_output_kind: String,
    /// Fields are validated for collection integrity but are not Office tables.
    pub data: Option<OfficeObjectData>,
}
pub struct OfficeSourceRegion<'a> { pub id: &'a str, pub kind: &'a str }
pub struct OfficeSourcePage<'a> {
    pub id: &'a str, pub provenance: Option<&'a Value>,
    pub regions: Vec<OfficeSourceRegion<'a>>,
}

fn error(message: &str) -> String { format!("Cannot export OCR Office content: {message}.") }
fn valid_text(value: &str, limit: usize) -> bool {
    value.encode_utf16().count() <= limit && !value.contains('\0')
}
fn valid_id(value: &str) -> bool { !value.trim().is_empty() && valid_text(value, 200) }
fn bounded_json(value: &Value, limit: usize, max_depth: usize) -> Result<(), String> {
    if serde_json::to_vec(value).map_err(|e| e.to_string())?.len() > limit {
        return Err(error("stored extraction data exceeds its byte limit; nothing was truncated"));
    }
    let mut pending = vec![(value, 0usize)];
    let mut count = 0;
    while let Some((value, depth)) = pending.pop() {
        count += 1;
        if count > 100_000 || depth > max_depth { return Err(error("original output is too deeply nested or complex")); }
        match value {
            Value::Array(values) => pending.extend(values.iter().map(|value| (value, depth + 1))),
            Value::Object(values) => pending.extend(values.values().map(|value| (value, depth + 1))),
            _ => (),
        }
    }
    Ok(())
}

/// XML 1.0 cannot represent these characters; fail before writing, never strip.
pub fn validate_office_text(value: &str) -> Result<(), String> {
    if let Some(ch) = value.chars().find(|ch| (*ch < ' ' && !['\t', '\n', '\r'].contains(ch)) || ['\u{fffe}', '\u{ffff}'].contains(ch)) {
        return Err(error(&format!("text contains XML-incompatible character U+{:04X}; correct it before exporting", ch as u32)));
    }
    Ok(())
}
fn validate_bounds(bounds: &OfficeBounds, width: f64, height: f64) -> bool {
    [bounds.x, bounds.y, bounds.w, bounds.h, width, height].iter().all(|v| v.is_finite()) &&
        bounds.x >= 0.0 && bounds.y >= 0.0 && bounds.w > 0.0 && bounds.h > 0.0 &&
        bounds.x + bounds.w <= width + 0.01 && bounds.y + bounds.h <= height + 0.01
}

/// Explicit cells must cover the declared grid exactly; no inferred cells/types.
pub fn validate_table(table: &OfficeTableData) -> Result<(), String> {
    if table.version != 1 || table.rows > 1_000 || table.columns > 1_000 ||
        table.rows.saturating_mul(table.columns) > 10_000 || table.cells.len() > 10_000 {
        return Err(error("unsupported table version or grid size"));
    }
    if (table.rows == 0 || table.columns == 0) && (table.rows != 0 || table.columns != 0 || !table.cells.is_empty()) {
        return Err(error("an empty table must be exactly 0 by 0"));
    }
    let mut occupied = vec![false; table.rows * table.columns];
    let mut identities = HashSet::new();
    let mut text_units = 0usize;
    for cell in &table.cells {
        if !valid_id(&cell.id) || !identities.insert(cell.id.as_str()) ||
            cell.row_span == 0 || cell.column_span == 0 ||
            cell.row.checked_add(cell.row_span).is_none_or(|end| end > table.rows) ||
            cell.column.checked_add(cell.column_span).is_none_or(|end| end > table.columns) ||
            cell.text.contains('\0') {
            return Err(error("invalid table cell identity, span, or text"));
        }
        text_units = text_units.saturating_add(cell.text.encode_utf16().count());
        if text_units > MAX_TEXT { return Err(error("table text exceeds 1,000,000 units")); }
        for row in cell.row..cell.row + cell.row_span {
            for column in cell.column..cell.column + cell.column_span {
                let slot = &mut occupied[row * table.columns + column];
                if *slot { return Err(error("table cell spans overlap")); }
                *slot = true;
            }
        }
        if let Some(source) = &cell.source_box {
            if !valid_id(&source.capture_id) || !validate_bounds(&source.bounds, 100_000.0, 100_000.0) {
                return Err(error("invalid measured cell box"));
            }
        }
    }
    if occupied.iter().any(|covered| !covered) { return Err(error("table cell spans leave an uncovered grid position")); }
    Ok(())
}
fn validate_captures(captures: &[OfficeCapture]) -> Result<(), String> {
    if captures.is_empty() || captures.len() > 128 { return Err(error("one object needs 1–128 bounded source captures")); }
    let mut identities = HashSet::new();
    for capture in captures {
        if !valid_id(&capture.id) || !identities.insert(capture.id.as_str()) || !valid_id(&capture.page_id) || !valid_id(&capture.document_id) ||
            capture.width == 0 || capture.height == 0 || capture.width > 100_000 || capture.height > 100_000 ||
            u64::from(capture.width) * u64::from(capture.height) > 100_000_000 ||
            capture.image_path.is_empty() || !valid_text(&capture.image_path, 10_000) {
            return Err(error("invalid source capture identity, dimensions, or image reference"));
        }
        match (&capture.region_id, &capture.bounds) {
            (None, None) => (),
            (Some(region_id), Some(bounds)) if valid_id(region_id) && validate_bounds(bounds, f64::from(capture.width), f64::from(capture.height)) => (),
            _ => return Err(error("source crop does not match its measured page or region identity")),
        }
        match (capture.method.as_str(), &capture.text) {
            ("pixels", None) => (),
            ("text", Some(text)) if valid_text(text, MAX_TEXT) => (),
            _ => return Err(error("invalid source capture method or source text")),
        }
    }
    Ok(())
}
fn validate_cell_sources(table: &OfficeTableData, captures: &[OfficeCapture]) -> Result<(), String> {
    for cell in &table.cells {
        if let Some(source) = &cell.source_box {
            let capture = captures.iter().find(|capture| capture.id == source.capture_id)
                .ok_or_else(|| error("measured table cell refers to a missing capture"))?;
            let limit = capture.bounds.clone().unwrap_or(OfficeBounds { x: 0.0, y: 0.0, w: f64::from(capture.width), h: f64::from(capture.height) });
            if capture.method != "pixels" || source.bounds.x < limit.x || source.bounds.y < limit.y ||
                source.bounds.x + source.bounds.w > limit.x + limit.w + 0.01 || source.bounds.y + source.bounds.h > limit.y + limit.h + 0.01 {
                return Err(error("measured table cell lies outside its actual pixel capture"));
            }
        }
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawCollection { version: u32, objects: Vec<RawObject> }
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawObject {
    version: u32, id: String, kind: String, name: String, created_at: String, updated_at: String,
    excluded: bool, review: String, captures: Vec<OfficeCapture>, original_output: Value,
    original_output_kind: String, original_data: Value, current_data: Value,
    #[serde(default)] table_sources: Option<TableSources>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FormulaData { version: u32, latex: String }
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FieldsData { version: u32, labels: Vec<String>, fields: Vec<FieldValue> }
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FieldValue { id: String, label: String, value: String, score: Option<f64> }
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TableSources { version: u32, snapshots: Vec<TableSnapshot> }
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TableSnapshot {
    id: String, object_id: String, owner_page_id: String, name: String,
    captured_at: String, source_updated_at: String, original_output_kind: String,
    original_output: Value, original_data: OfficeTableData,
    current_data: OfficeTableData, captures: Vec<OfficeCapture>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct JoinReceipt {
    version: u32, operation: String, target_columns: usize,
    inputs: Vec<JoinInput>, cell_origins: Vec<CellOrigin>, blank_cell_ids: Vec<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct JoinInput { snapshot_id: String, drop_header_rows: usize, column_map: Vec<usize> }
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CellOrigin { cell_id: String, snapshot_id: String, source_cell_id: String }

fn from_value<T: serde::de::DeserializeOwned>(value: &Value) -> Result<T, String> {
    serde_json::from_value(value.clone()).map_err(|e| error(&format!("invalid versioned extraction data: {e}")))
}
fn validate_formula(value: &Value) -> Result<String, String> {
    let formula: FormulaData = from_value(value)?;
    if formula.version != 1 || !valid_text(&formula.latex, MAX_TEXT) { return Err(error("invalid bounded literal LaTeX")); }
    Ok(formula.latex)
}
fn validate_fields(value: &Value) -> Result<FieldsData, String> {
    let fields: FieldsData = from_value(value)?;
    if fields.version != 1 || fields.labels.len() > 1_000 || fields.fields.len() > 1_000 || fields.labels.iter().any(|label| !valid_text(label, 10_000)) {
        return Err(error("invalid field collection"));
    }
    let mut identities = HashSet::new();
    let mut units: usize = fields.labels.iter().map(|label| label.encode_utf16().count()).sum();
    for field in &fields.fields {
        if !valid_id(&field.id) || !identities.insert(field.id.as_str()) || !valid_text(&field.label, 10_000) || !valid_text(&field.value, MAX_TEXT) ||
            field.score.is_some_and(|score| !score.is_finite() || !(0.0..=1.0).contains(&score)) { return Err(error("invalid field identity, text, or model score")); }
        units += field.label.encode_utf16().count() + field.value.encode_utf16().count();
    }
    if units > MAX_TEXT { return Err(error("field text exceeds 1,000,000 units")); }
    Ok(fields)
}
fn read_receipt(value: &Value) -> Result<JoinReceipt, String> {
    let receipt: JoinReceipt = from_value(value)?;
    if receipt.version != 1 || receipt.operation != "continuation-join" || !(1..=1_000).contains(&receipt.target_columns) ||
        !(2..=16).contains(&receipt.inputs.len()) || receipt.cell_origins.len() > 10_000 || receipt.blank_cell_ids.len() > 10_000 {
        return Err(error("invalid continuation-table receipt"));
    }
    let mut sources = HashSet::new();
    for (index, input) in receipt.inputs.iter().enumerate() {
        if !valid_id(&input.snapshot_id) || !sources.insert(input.snapshot_id.as_str()) || input.drop_header_rows > 1_000 ||
            (index == 0 && input.drop_header_rows != 0) || input.column_map.is_empty() || input.column_map.len() > 1_000 ||
            input.column_map.iter().any(|column| *column >= receipt.target_columns) ||
            input.column_map.iter().collect::<HashSet<_>>().len() != input.column_map.len() {
            return Err(error("invalid continuation mapping or repeated-header removal"));
        }
    }
    let mut cells = HashSet::new();
    for origin in &receipt.cell_origins {
        if !valid_id(&origin.cell_id) || !valid_id(&origin.snapshot_id) || !valid_id(&origin.source_cell_id) || !cells.insert(origin.cell_id.as_str()) {
            return Err(error("invalid or duplicate joined-cell origin"));
        }
    }
    for id in &receipt.blank_cell_ids {
        if !valid_id(id) || !cells.insert(id.as_str()) { return Err(error("joined cell origins and blank positions overlap")); }
    }
    Ok(receipt)
}
fn validate_join_baseline(baseline: &OfficeTableData, receipt: &JoinReceipt, sources: &HashMap<&str, &TableSnapshot>) -> Result<(), String> {
    if baseline.columns != receipt.target_columns { return Err(error("joined baseline width differs from its mapping")); }
    let by_id: HashMap<_, _> = baseline.cells.iter().map(|cell| (cell.id.as_str(), cell)).collect();
    let mut origins = HashMap::new();
    for origin in &receipt.cell_origins {
        if !receipt.inputs.iter().any(|input| input.snapshot_id == origin.snapshot_id) ||
            origins.insert((origin.snapshot_id.as_str(), origin.source_cell_id.as_str()), origin).is_some() {
            return Err(error("joined cell duplicates or invents a source"));
        }
    }
    let mut offset = 0usize;
    let mut accounted = HashSet::new();
    for input in &receipt.inputs {
        let table = &sources.get(input.snapshot_id.as_str()).ok_or_else(|| error("joined input snapshot is missing"))?.current_data;
        if table.rows == 0 || table.columns == 0 || input.drop_header_rows > table.rows || input.column_map.len() != table.columns {
            return Err(error("joined input or mapping has incompatible dimensions"));
        }
        for source in &table.cells {
            if source.row + source.row_span <= input.drop_header_rows { continue; }
            if source.row < input.drop_header_rows { return Err(error("header removal cuts through a source span")); }
            let mut mapped = input.column_map[source.column..source.column + source.column_span].to_vec();
            mapped.sort_unstable();
            if mapped.windows(2).any(|pair| pair[1] != pair[0] + 1) { return Err(error("a source span maps to disconnected columns")); }
            let origin = origins.remove(&(input.snapshot_id.as_str(), source.id.as_str())).ok_or_else(|| error("joined baseline omits a source cell"))?;
            let cell = by_id.get(origin.cell_id.as_str()).ok_or_else(|| error("joined cell origin is missing from its baseline"))?;
            if cell.row != offset + source.row - input.drop_header_rows || cell.column != mapped[0] ||
                cell.row_span != source.row_span || cell.column_span != source.column_span || cell.text != source.text ||
                cell.header != source.header || cell.source_box != source.source_box {
                return Err(error("joined baseline changed an input cell or measured source box"));
            }
            accounted.insert(origin.cell_id.as_str());
        }
        offset += table.rows - input.drop_header_rows;
    }
    if offset != baseline.rows || !origins.is_empty() { return Err(error("joined baseline contains missing or unexpected source cells")); }
    for id in &receipt.blank_cell_ids {
        let cell = by_id.get(id.as_str()).ok_or_else(|| error("joined blank cell is missing"))?;
        if !cell.text.is_empty() || cell.row_span != 1 || cell.column_span != 1 || cell.header.is_some() || cell.source_box.is_some() {
            return Err(error("joined unmapped positions must be literal empty cells without inferred metadata"));
        }
        accounted.insert(id.as_str());
    }
    if accounted.len() != baseline.cells.len() { return Err(error("join receipt does not account for every baseline cell")); }
    Ok(())
}
fn valid_output_kind(kind: &str) -> bool { ["model", "legacy-record", "table-join"].contains(&kind) }
// Timestamps are retained opaque; the writer does not infer order from dates.
fn valid_stamp(value: &str) -> bool { !value.trim().is_empty() && valid_text(value, 64) }
fn validate_join(baseline: &OfficeTableData, receipt: &JoinReceipt, history: &TableSources, captures: &[OfficeCapture]) -> Result<(), String> {
    if history.version != 1 || history.snapshots.is_empty() || history.snapshots.len() > 128 { return Err(error("invalid flat table-source history")); }
    let mut sources = HashMap::new();
    let mut receipts = HashMap::new();
    let mut capture_ids = HashSet::new();
    for snapshot in &history.snapshots {
        if !valid_id(&snapshot.id) || sources.insert(snapshot.id.as_str(), snapshot).is_some() || !valid_id(&snapshot.object_id) ||
            !valid_id(&snapshot.owner_page_id) || !valid_text(&snapshot.name, 1_000) || !valid_stamp(&snapshot.captured_at) ||
            !valid_stamp(&snapshot.source_updated_at) || !valid_output_kind(&snapshot.original_output_kind) {
            return Err(error("invalid or duplicate table source snapshot"));
        }
        validate_table(&snapshot.original_data)?; validate_table(&snapshot.current_data)?;
        validate_captures(&snapshot.captures)?;
        validate_cell_sources(&snapshot.original_data, &snapshot.captures)?;
        validate_cell_sources(&snapshot.current_data, &snapshot.captures)?;
        bounded_json(&snapshot.original_output, MAX_RAW_BYTES, 32)?;
        for capture in &snapshot.captures {
            if captures.iter().find(|candidate| candidate.id == capture.id) != Some(capture) { return Err(error("joined captures are missing or conflicting")); }
            capture_ids.insert(capture.id.as_str());
        }
        if snapshot.original_output_kind == "table-join" { receipts.insert(snapshot.id.as_str(), read_receipt(&snapshot.original_output)?); }
    }
    if capture_ids.len() != captures.len() { return Err(error("joined captures include unrelated source records")); }
    let mut degree: HashMap<_, usize> = sources.keys().map(|id| (*id, 0)).collect();
    for child in receipts.values().flat_map(|receipt| &receipt.inputs) {
        *degree.get_mut(child.snapshot_id.as_str()).ok_or_else(|| error("joined ancestor snapshot is missing"))? += 1;
    }
    let mut ready: Vec<_> = degree.iter().filter_map(|(id, count)| (*count == 0).then_some(*id)).collect();
    let mut visited = 0;
    while let Some(id) = ready.pop() {
        visited += 1;
        if let Some(receipt) = receipts.get(id) {
            for child in &receipt.inputs {
                let count = degree.get_mut(child.snapshot_id.as_str()).unwrap();
                *count -= 1;
                if *count == 0 { ready.push(child.snapshot_id.as_str()); }
            }
        }
    }
    if visited != sources.len() { return Err(error("table source receipts contain a cycle")); }
    let mut reachable = HashSet::new();
    let mut pending: Vec<_> = receipt.inputs.iter().map(|input| input.snapshot_id.as_str()).collect();
    while let Some(id) = pending.pop() {
        if !sources.contains_key(id) { return Err(error("joined input snapshot is missing")); }
        if !reachable.insert(id) { continue; }
        if let Some(receipt) = receipts.get(id) { pending.extend(receipt.inputs.iter().map(|input| input.snapshot_id.as_str())); }
    }
    if reachable.len() != sources.len() { return Err(error("table source history contains unreferenced snapshots")); }
    for (id, receipt) in receipts { validate_join_baseline(&sources[id].original_data, &receipt, &sources)?; }
    validate_join_baseline(baseline, receipt, &sources)
}

/// Validate the complete collection before resolving a selected stable ID.
pub fn read_extraction_collection(value: &Value) -> Result<Vec<ValidatedExtractionObject>, String> {
    if serde_json::to_vec(value).map_err(|e| e.to_string())?.len() > MAX_COLLECTION_BYTES {
        return Err(error("extraction collection exceeds 16 MB"));
    }
    let collection: RawCollection = from_value(value)?;
    if collection.version != 1 || collection.objects.len() > 128 { return Err(error("unsupported collection version or object count")); }
    let mut identities = HashSet::new();
    let mut result = vec![];
    for object in collection.objects {
        if object.version != 1 || !valid_id(&object.id) || !identities.insert(object.id.clone()) || !valid_text(&object.name, 1_000) ||
            !valid_stamp(&object.created_at) || !valid_stamp(&object.updated_at) || !["unreviewed", "corrected", "approved"].contains(&object.review.as_str()) ||
            !valid_output_kind(&object.original_output_kind) { return Err(error("invalid object identity, version, or metadata")); }
        if (object.original_output_kind == "table-join") != object.table_sources.is_some() ||
            (object.table_sources.is_some() && object.kind != "table") { return Err(error("only joined tables may and must contain flat source history")); }
        validate_captures(&object.captures)?;
        bounded_json(&object.original_output, MAX_RAW_BYTES, 32)?;
        let data = match object.kind.as_str() {
            "table" => {
                let original: OfficeTableData = from_value(&object.original_data)?;
                let current: OfficeTableData = from_value(&object.current_data)?;
                validate_table(&original)?; validate_table(&current)?;
                validate_cell_sources(&original, &object.captures)?; validate_cell_sources(&current, &object.captures)?;
                if let Some(history) = &object.table_sources { validate_join(&original, &read_receipt(&object.original_output)?, history, &object.captures)?; }
                Some(OfficeObjectData::Table(current))
            },
            "formula" => { validate_formula(&object.original_data)?; Some(OfficeObjectData::Formula(validate_formula(&object.current_data)?)) },
            "fields" => {
                let original = validate_fields(&object.original_data)?;
                let current = validate_fields(&object.current_data)?;
                for field in &current.fields {
                    if field.score.is_some() && !original.fields.iter().any(|prior| prior.id == field.id && prior.label == field.label && prior.value == field.value && prior.score == field.score) {
                        return Err(error("edited fields retain an unrelated model score"));
                    }
                }
                None
            },
            _ => return Err(error("unsupported extraction kind")),
        };
        result.push(ValidatedExtractionObject { id: object.id, name: object.name, kind: object.kind,
            excluded: object.excluded, captures: object.captures, original_output_kind: object.original_output_kind, data });
    }
    Ok(result)
}

/// Resolve only supplied included pages and explicit object selections.
/// Selection order is retained. Duplicate replacement targets are rejected.
pub fn select_office_objects(pages: &[OfficeSourcePage<'_>], options: &OfficeOptions) -> Result<Vec<(String, OfficeExportObject)>, String> {
    if options.version != 1 || options.objects.len() > MAX_OFFICE_OBJECTS { return Err(error("unsupported Office options version or 512-object limit")); }
    let mut page_ids = HashSet::new();
    for page in pages {
        if !valid_id(page.id) || !page_ids.insert(page.id) { return Err(error("Office page identities are missing or repeated")); }
        let mut regions = HashSet::new();
        if page.regions.iter().any(|region| !valid_id(region.id) || !regions.insert(region.id)) { return Err(error("Office source region identities are missing or repeated")); }
    }
    let mut collections: HashMap<&str, Vec<ValidatedExtractionObject>> = HashMap::new();
    let mut selected = HashSet::new();
    let mut replacements = HashSet::new();
    let mut result = vec![];
    for selection in &options.objects {
        if !valid_id(&selection.page_id) || !valid_id(&selection.object_id) || !selected.insert((selection.page_id.as_str(), selection.object_id.as_str())) {
            return Err(error("selected extraction identities are invalid or repeated"));
        }
        let page = pages.iter().find(|page| page.id == selection.page_id).ok_or_else(|| error("a selected extraction belongs to a page outside this export scope"))?;
        if !collections.contains_key(page.id) {
            let value = page.provenance.and_then(|provenance| provenance.get("extractionObjects"))
                .ok_or_else(|| error("selected extraction data is missing; reopen the selection"))?;
            collections.insert(page.id, read_extraction_collection(value)?);
        }
        let object = collections[page.id].iter().find(|object| object.id == selection.object_id)
            .ok_or_else(|| error("selected extraction no longer exists"))?;
        if object.excluded { return Err(error("an excluded extraction cannot be selected for Office export")); }
        let data = object.data.clone().ok_or_else(|| error("field sets cannot be exported as Word tables or formulas"))?;
        match &data {
            OfficeObjectData::Table(table) => {
                if table.rows == 0 { return Err(error("selected table is empty; add cells or remove it from this export")); }
                for cell in &table.cells { validate_office_text(&cell.text)?; }
            },
            OfficeObjectData::Formula(latex) => validate_office_text(latex)?,
        }
        validate_office_text(&object.name)?;
        let placement = match (selection.placement.as_str(), &selection.region_id) {
            ("append", None) => OfficePlacement::Append,
            ("replace-region", Some(region_id)) => {
                if !page.regions.iter().any(|region| region.id == region_id && region.kind == object.kind) {
                    return Err(error("replacement needs an included source region of the same table/formula kind"));
                }
                if !replacements.insert((page.id, region_id.as_str())) { return Err(error("multiple selected extractions replace the same source region")); }
                OfficePlacement::ReplaceRegion { region_id: region_id.clone() }
            },
            _ => return Err(error("choose explicit append or replacement placement for every selected extraction")),
        };
        result.push((page.id.to_owned(), OfficeExportObject { object_id: object.id.clone(), name: object.name.clone(), placement, data }));
    }
    Ok(result)
}

/// Resolve an explicit table-only workbook without inspecting current page
/// pixels or requiring current OCR passages. The writer applies Excel-specific
/// name/cell limits; this shared boundary applies provenance and scope rules.
pub fn select_workbook_tables(pages: &[OfficeSourcePage<'_>], options: &WorkbookOptions) -> Result<Vec<OfficeWorkbookSheet>, String> {
    if options.version != 1 || options.sheets.is_empty() || options.sheets.len() > MAX_OFFICE_OBJECTS {
        return Err(error("workbook options must use version 1 and select 1–512 saved tables"));
    }
    if pages.len() > 10_000 { return Err(error("workbook source scope exceeds 10,000 pages")); }
    let mut page_ids = HashSet::new();
    for page in pages {
        if !valid_id(page.id) || !page_ids.insert(page.id) { return Err(error("workbook page identities are missing or repeated")); }
    }
    let mut collections: HashMap<&str, Vec<ValidatedExtractionObject>> = HashMap::new();
    let mut selected = HashSet::new();
    let mut result = Vec::with_capacity(options.sheets.len());
    let mut grid_positions = 0usize;
    let mut text_bytes = 0usize;
    for choice in &options.sheets {
        if !valid_id(&choice.page_id) || !valid_id(&choice.object_id) || !selected.insert((choice.page_id.as_str(), choice.object_id.as_str())) {
            return Err(error("workbook table identities are invalid or repeated"));
        }
        if !valid_text(&choice.sheet_name, 1_000) { return Err(error("worksheet name is invalid or exceeds the input bound")); }
        validate_office_text(&choice.sheet_name)?;
        let page = pages.iter().find(|page| page.id == choice.page_id)
            .ok_or_else(|| error("a selected table belongs to a page outside this workbook scope"))?;
        if !collections.contains_key(page.id) {
            let value = page.provenance.and_then(|provenance| provenance.get("extractionObjects"))
                .ok_or_else(|| error("selected workbook table data is missing; reopen the selection"))?;
            let requested: HashSet<_> = options.sheets.iter().filter(|choice| choice.page_id == page.id)
                .map(|choice| choice.object_id.as_str()).collect();
            let objects = read_extraction_collection(value)?.into_iter()
                .filter(|object| requested.contains(object.id.as_str())).collect();
            collections.insert(page.id, objects);
        }
        let object = collections[page.id].iter().find(|object| object.id == choice.object_id)
            .ok_or_else(|| error("selected workbook table no longer exists"))?;
        if object.excluded { return Err(error("an excluded table cannot be selected for workbook export")); }
        let Some(OfficeObjectData::Table(table)) = &object.data else {
            return Err(error("workbook worksheets require saved tables; formulas and field sets are not tables"));
        };
        grid_positions = grid_positions.saturating_add(table.rows * table.columns);
        if grid_positions > MAX_WORKBOOK_GRID_POSITIONS {
            return Err(error("selected tables exceed 1,000,000 total grid positions; export fewer tables"));
        }
        for cell in &table.cells {
            validate_office_text(&cell.text)?;
            text_bytes = text_bytes.saturating_add(cell.text.len());
            if text_bytes > MAX_WORKBOOK_TEXT_BYTES {
                return Err(error("selected workbook cell text exceeds 64 MiB; export fewer tables"));
            }
        }
        result.push(OfficeWorkbookSheet { page_id: page.id.to_owned(), object_id: object.id.clone(), name: choice.sheet_name.clone(), table: table.clone() });
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn table(text: &str) -> Value { json!({"version":1,"rows":1,"columns":1,"cells":[{"id":"cell","row":0,"column":0,"rowSpan":1,"columnSpan":1,"text":text}]}) }
    fn object(text: &str) -> Value {
        json!({"version":1,"id":"table","kind":"table","name":"Measurements","createdAt":"2026-11-09T00:00:00.000Z","updatedAt":"2026-11-09T00:00:00.000Z",
            "excluded":false,"review":"corrected","captures":[{"id":"capture","pageId":"page","documentId":"document","width":20,"height":20,
                "imagePath":"/never/open/a/provenance/path.png","regionId":"region","bounds":{"x":1,"y":1,"w":10,"h":10},"method":"pixels","text":null}],
            "originalOutput":{"html":"<table><td>original</td></table>"},"originalOutputKind":"model","originalData":table("original"),"currentData":table(text)})
    }
    fn collection(entry: Value) -> Value { json!({"version":1,"objects":[entry]}) }
    fn selection(placement: &str, region: Option<&str>) -> OfficeOptions {
        OfficeOptions { version: 1, objects: vec![OfficeObjectSelection { page_id: "page".into(), object_id: "table".into(), placement: placement.into(), region_id: region.map(str::to_owned) }] }
    }
    fn workbook_selection(page_id: &str, object_id: &str, sheet_name: &str) -> WorkbookSheetSelection {
        WorkbookSheetSelection { page_id: page_id.into(), object_id: object_id.into(), sheet_name: sheet_name.into() }
    }
    #[test]
    fn workbook_selects_only_current_literal_tables_in_explicit_tab_order() {
        let literal = " 0012\t=SUM(A1)\r\n_x000D_ 😀 & < > ";
        let first = json!({"extractionObjects":collection(object(literal))});
        let mut empty = object("old");
        empty["currentData"] = json!({"version":1,"rows":0,"columns":0,"cells":[]});
        let second = json!({"extractionObjects":collection(empty)});
        let malformed_unselected = json!({"extractionObjects":{"version":999}});
        let pages = [OfficeSourcePage { id:"page", provenance:Some(&first), regions:vec![] },
            OfficeSourcePage { id:"empty-page", provenance:Some(&second), regions:vec![] },
            OfficeSourcePage { id:"unselected-page", provenance:Some(&malformed_unselected), regions:vec![] }];
        let options = WorkbookOptions { version:1, sheets:vec![
            workbook_selection("empty-page","table","Empty capture"), workbook_selection("page","table","Measurements"),
        ] };
        let result = select_workbook_tables(&pages, &options).unwrap();
        assert_eq!(result.iter().map(|sheet| sheet.name.as_str()).collect::<Vec<_>>(), ["Empty capture","Measurements"]);
        assert_eq!((result[0].table.rows,result[0].table.columns,result[0].table.cells.len()),(0,0,0));
        assert_eq!(result[1].table.cells[0].text,literal);
        assert_eq!(result[1].page_id,"page");
        assert_eq!(result[1].object_id,"table");
        assert_eq!(first["extractionObjects"]["objects"][0]["originalData"]["cells"][0]["text"],"original");
    }
    #[test]
    fn workbook_retains_tables_wider_than_word_without_inventing_source_cells() {
        let mut entry = object("original");
        entry["currentData"] = json!({"version":1,"rows":1,"columns":64,"cells":(0..64).map(|column|
            json!({"id":format!("cell-{column}"),"row":0,"column":column,"rowSpan":1,"columnSpan":1,"text":format!("{column:04}")})).collect::<Vec<_>>()});
        let provenance=json!({"extractionObjects":collection(entry)});
        let pages=[OfficeSourcePage{id:"page",provenance:Some(&provenance),regions:vec![]}];
        let options=WorkbookOptions{version:1,sheets:vec![workbook_selection("page","table","Wide")]};
        let result=select_workbook_tables(&pages,&options).unwrap();
        assert_eq!(result[0].table.columns,64);
        assert_eq!(result[0].table.cells[63].text,"0063");
    }
    #[test]
    fn workbook_rejects_scope_exclusion_non_tables_and_bad_identities() {
        let options=WorkbookOptions{version:1,sheets:vec![workbook_selection("page","table","Values")]};
        assert!(select_workbook_tables(&[],&options).unwrap_err().contains("scope"));
        let mut excluded=object("value");excluded["excluded"]=json!(true);
        let provenance=json!({"extractionObjects":collection(excluded)});
        let pages=[OfficeSourcePage{id:"page",provenance:Some(&provenance),regions:vec![]}];
        assert!(select_workbook_tables(&pages,&options).unwrap_err().contains("excluded"));
        let mut formula=object("value");formula["kind"]=json!("formula");
        formula["originalData"]=json!({"version":1,"latex":"x"});formula["currentData"]=json!({"version":1,"latex":"y"});
        let provenance=json!({"extractionObjects":collection(formula)});
        let pages=[OfficeSourcePage{id:"page",provenance:Some(&provenance),regions:vec![]}];
        assert!(select_workbook_tables(&pages,&options).unwrap_err().contains("formulas and field sets"));
        let missing=[OfficeSourcePage{id:"",provenance:None,regions:vec![]}];
        assert!(select_workbook_tables(&missing,&options).unwrap_err().contains("identities"));
        let duplicated=[OfficeSourcePage{id:"page",provenance:None,regions:vec![]},OfficeSourcePage{id:"page",provenance:None,regions:vec![]}];
        assert!(select_workbook_tables(&duplicated,&options).unwrap_err().contains("identities"));
    }
    #[test]
    fn workbook_requires_versioned_bounded_explicit_choices() {
        assert!(serde_json::from_value::<WorkbookOptions>(json!({"version":1,"sheets":[],"objects":[]})).is_err());
        assert!(serde_json::from_value::<WorkbookOptions>(json!({"version":1,"sheets":[{"pageId":"page","objectId":"table","sheetName":"Values","placement":"append"}]})).is_err());
        let provenance=json!({"extractionObjects":collection(object("literal"))});
        let pages=[OfficeSourcePage{id:"page",provenance:Some(&provenance),regions:vec![]}];
        let choice=workbook_selection("page","table","Values");
        for options in [WorkbookOptions::default(),WorkbookOptions{version:2,sheets:vec![choice.clone()]},
            WorkbookOptions{version:1,sheets:vec![choice.clone();513]}] {
            assert!(select_workbook_tables(&pages,&options).unwrap_err().contains("1–512"));
        }
        assert!(select_workbook_tables(&pages,&WorkbookOptions{version:1,sheets:vec![choice.clone(),choice]}).unwrap_err().contains("repeated"));
    }
    #[test]
    fn exports_literal_current_values_without_reading_capture_paths() {
        let value = collection(object(" 0012\r\n=SUM(A1) 😀 & < > "));
        let provenance = json!({"extractionObjects":value});
        let pages = [OfficeSourcePage { id: "page", provenance: Some(&provenance), regions: vec![OfficeSourceRegion { id: "region", kind: "table" }] }];
        assert!(select_office_objects(&pages, &OfficeOptions::default()).unwrap().is_empty());
        let selected = select_office_objects(&pages, &selection("replace-region", Some("region"))).unwrap();
        let OfficeObjectData::Table(table) = &selected[0].1.data else { panic!("table missing") };
        assert_eq!(table.cells[0].text, " 0012\r\n=SUM(A1) 😀 & < > ");
        assert_eq!(selected[0].1.placement, OfficePlacement::ReplaceRegion { region_id: "region".into() });
    }
    #[test]
    fn rejects_unknown_versions_keys_duplicates_and_excluded_selections() {
        let value = collection(object("value"));
        for bad in [json!({"version":2,"objects":[]}), json!({"version":1,"objects":[],"extra":true}),
            json!({"version":1,"objects":[value["objects"][0].clone(),value["objects"][0].clone()]})] { assert!(read_extraction_collection(&bad).is_err()); }
        let mut entry = object("value"); entry["excluded"] = json!(true);
        let provenance = json!({"extractionObjects":collection(entry)});
        let pages = [OfficeSourcePage { id: "page", provenance: Some(&provenance), regions: vec![] }];
        assert!(select_office_objects(&pages, &selection("append", None)).unwrap_err().contains("excluded"));
        assert!(select_office_objects(&[], &selection("append", None)).unwrap_err().contains("scope"));
    }
    #[test]
    fn requires_explicit_compatible_unique_placement() {
        let mut second = object("second"); second["id"] = json!("table2");
        let provenance = json!({"extractionObjects":{"version":1,"objects":[object("first"),second]}});
        let pages = [OfficeSourcePage { id: "page", provenance: Some(&provenance), regions: vec![OfficeSourceRegion { id: "region", kind: "table" },OfficeSourceRegion { id: "paragraph", kind: "text" }] }];
        assert!(select_office_objects(&pages, &selection("append", Some("region"))).is_err());
        assert!(select_office_objects(&pages, &selection("replace-region", Some("paragraph"))).is_err());
        let mut options = selection("replace-region", Some("region"));
        let mut second = options.objects[0].clone(); second.object_id = "table2".into(); options.objects.push(second);
        assert!(select_office_objects(&pages, &options).unwrap_err().contains("same source"));
        options.objects[1].placement = "append".into(); options.objects[1].region_id = None;
        assert_eq!(select_office_objects(&pages, &options).unwrap().iter().map(|(_, object)| object.object_id.as_str()).collect::<Vec<_>>(), vec!["table", "table2"]);
    }
    #[test]
    fn table_grid_and_real_source_boxes_are_independently_validated() {
        let mut entry = object("value");
        entry["currentData"]["cells"][0]["box"] = json!({"captureId":"capture","bounds":{"x":2,"y":2,"w":2,"h":2}});
        read_extraction_collection(&collection(entry.clone())).unwrap();
        entry["currentData"]["cells"][0]["box"]["bounds"]["x"] = json!(15);
        assert!(read_extraction_collection(&collection(entry)).unwrap_err().contains("outside"));
        let base: OfficeTableData = from_value(&table("literal")).unwrap();
        let mut bad = base.clone(); bad.cells[0].row_span = usize::MAX; assert!(validate_table(&bad).is_err());
        let mut bad = base.clone(); bad.columns = 2; assert!(validate_table(&bad).unwrap_err().contains("uncovered"));
        let mut bad = base.clone(); let mut cell = bad.cells[0].clone(); cell.id = "other".into(); bad.cells.push(cell); assert!(validate_table(&bad).unwrap_err().contains("overlap"));
        let mut bad = base; bad.cells[0].text = "😀".repeat(500_001); assert!(validate_table(&bad).is_err());
    }
    #[test]
    fn rejects_xml_controls_without_stripping_literal_strings() {
        for text in ["\u{0001}", "\u{fffe}", "\u{ffff}"] { assert!(validate_office_text(text).is_err()); }
        validate_office_text("\t\r\n😀 =+@- & < > _x0041_").unwrap();
    }
    #[test]
    fn formula_selection_keeps_exact_latex_and_field_corrections_cannot_keep_scores() {
        let mut formula = object("unused"); formula["kind"] = json!("formula");
        formula["originalData"] = json!({"version":1,"latex":"x"});
        formula["currentData"] = json!({"version":1,"latex":" \\frac{0012}{x} + 😀\r\n"});
        let parsed = read_extraction_collection(&collection(formula)).unwrap();
        assert_eq!(parsed[0].data, Some(OfficeObjectData::Formula(" \\frac{0012}{x} + 😀\r\n".into())));
        let mut fields = object("unused"); fields["kind"] = json!("fields");
        fields["originalData"] = json!({"version":1,"labels":["Total"],"fields":[{"id":"field","label":"Total","value":"0012","score":0.8}]});
        fields["currentData"] = fields["originalData"].clone();
        fields["currentData"]["fields"][0]["value"] = json!("0013");
        assert!(read_extraction_collection(&collection(fields.clone())).unwrap_err().contains("unrelated model score"));
        fields["currentData"]["fields"][0]["score"] = Value::Null;
        assert!(read_extraction_collection(&collection(fields)).unwrap()[0].data.is_none());
    }
    #[test]
    fn original_output_and_selection_limits_fail_without_truncation() {
        let mut entry = object("value");
        let mut nested = json!("deep");
        for _ in 0..34 { nested = json!([nested]); }
        entry["originalOutput"] = nested;
        assert!(read_extraction_collection(&collection(entry)).unwrap_err().contains("deeply nested"));
        let mut options = OfficeOptions::default();
        options.objects = vec![selection("append", None).objects.remove(0); MAX_OFFICE_OBJECTS + 1];
        assert!(select_office_objects(&[], &options).unwrap_err().contains("512-object"));
        let empty = json!({"version":1,"rows":0,"columns":0,"cells":[]});
        let mut entry = object("value"); entry["currentData"] = empty;
        let provenance = json!({"extractionObjects":collection(entry)});
        let pages = [OfficeSourcePage { id: "page", provenance: Some(&provenance), regions: vec![] }];
        assert!(select_office_objects(&pages, &selection("append", None)).unwrap_err().contains("empty"));
    }
    fn joined() -> Value {
        let first = object("0012"); let mut second = object("0007");
        second["id"] = json!("table2"); second["captures"][0]["id"] = json!("capture2");
        let snapshot = |id: &str, input: &Value| json!({"id":id,"objectId":input["id"],"ownerPageId":"page","name":input["name"],
            "capturedAt":input["updatedAt"],"sourceUpdatedAt":input["updatedAt"],"originalOutputKind":input["originalOutputKind"],
            "originalOutput":input["originalOutput"],"originalData":input["originalData"],"currentData":input["currentData"],"captures":input["captures"]});
        let mut result = object("0012"); result["id"] = json!("joined"); result["originalOutputKind"] = json!("table-join");
        result["captures"] = json!([first["captures"][0].clone(),second["captures"][0].clone()]);
        result["originalOutput"] = json!({"version":1,"operation":"continuation-join","targetColumns":1,
            "inputs":[{"snapshotId":"s1","dropHeaderRows":0,"columnMap":[0]},{"snapshotId":"s2","dropHeaderRows":0,"columnMap":[0]}],
            "cellOrigins":[{"cellId":"out1","snapshotId":"s1","sourceCellId":"cell"},{"cellId":"out2","snapshotId":"s2","sourceCellId":"cell"}],"blankCellIds":[]});
        result["originalData"] = json!({"version":1,"rows":2,"columns":1,"cells":[
            {"id":"out1","row":0,"column":0,"rowSpan":1,"columnSpan":1,"text":"0012"},
            {"id":"out2","row":1,"column":0,"rowSpan":1,"columnSpan":1,"text":"0007"}]});
        result["currentData"] = result["originalData"].clone();
        result["tableSources"] = json!({"version":1,"snapshots":[snapshot("s1",&first),snapshot("s2",&second)]});
        result
    }
    #[test]
    fn validates_flat_join_ancestry_and_exact_manual_baseline() {
        let mut entry = joined();
        entry["currentData"]["cells"][0]["text"] = json!("later correction");
        read_extraction_collection(&collection(entry.clone())).unwrap();
        entry["originalData"]["cells"][0]["text"] = json!("forged baseline");
        assert!(read_extraction_collection(&collection(entry)).unwrap_err().contains("changed an input"));
        let mut entry = joined(); entry["originalOutput"]["inputs"][1]["columnMap"] = json!([1]);
        assert!(read_extraction_collection(&collection(entry)).is_err());
        let mut entry = joined(); entry["tableSources"]["snapshots"][0]["tableSources"] = json!({"version":1,"snapshots":[]});
        assert!(read_extraction_collection(&collection(entry)).is_err());
        let mut entry = joined(); entry["tableSources"]["snapshots"][0]["captures"][0]["imagePath"] = json!("/different.png");
        assert!(read_extraction_collection(&collection(entry)).unwrap_err().contains("conflicting"));
    }
    #[test]
    fn joined_receipts_reject_cycles_missing_and_unreferenced_sources() {
        let mut entry = joined();
        entry["tableSources"]["snapshots"][0]["originalOutputKind"] = json!("table-join");
        entry["tableSources"]["snapshots"][0]["originalOutput"] = entry["originalOutput"].clone();
        assert!(read_extraction_collection(&collection(entry)).unwrap_err().contains("cycle"));
        let mut entry = joined(); entry["originalOutput"]["inputs"][1]["snapshotId"] = json!("missing");
        assert!(read_extraction_collection(&collection(entry)).unwrap_err().contains("missing"));
        let mut entry = joined(); let mut extra = entry["tableSources"]["snapshots"][0].clone(); extra["id"] = json!("unreferenced");
        entry["tableSources"]["snapshots"].as_array_mut().unwrap().push(extra);
        assert!(read_extraction_collection(&collection(entry)).unwrap_err().contains("unreferenced"));
    }
}
