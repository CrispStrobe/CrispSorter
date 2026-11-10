//! Logical OCR document semantics. Source regions remain the only text authority.
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DocumentStructure {
    pub version: u32,
    pub sections: Vec<StructureSection>,
    pub blocks: Vec<StructureBlock>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StructureSection { pub id: String, pub columns: u8 }
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StructureBlock {
    pub id: String,
    pub source_region_id: String,
    pub section_id: String,
    pub column: u8,
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heading_level: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub list: Option<StructureList>,
    pub alignment: String,
    pub break_before: String,
    pub runs: Vec<InlineRun>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor_block_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note_references: Option<Vec<NoteReference>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alt_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decorative: Option<bool>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StructureList {
    pub kind: String,
    pub level: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<u32>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InlineRun {
    pub start: usize,
    pub end: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bold: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub italic: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub underline: Option<bool>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NoteReference { pub start: usize, pub end: usize, pub target_block_id: String }

pub struct StructureSource<'a> { pub id: &'a str, pub text: &'a str, pub kind: &'a str }

fn valid_id(value: &str) -> bool {
    !value.trim().is_empty() && value.encode_utf16().count() <= 200 && !value.contains('\0')
}
fn fail(message: &str) -> Result<(), String> { Err(format!("Invalid document structure: {message}.")) }
fn boundaries(text: &str) -> HashMap<usize,usize> {
    let mut result = HashMap::from([(0,0)]);
    let mut offset = 0;
    for (byte,grapheme) in text.grapheme_indices(true) {
        offset += grapheme.encode_utf16().count();
        result.insert(offset,byte+grapheme.len());
    }
    result
}
fn textual(role: &str) -> bool {
    ["paragraph","heading","list-item","caption","header","footer","footnote"].contains(&role)
}
fn same_marks(a: &InlineRun, b: &InlineRun) -> bool {
    a.bold == b.bold && a.italic == b.italic && a.underline == b.underline
}

/// Validate JSON semantics before exporting them or interpreting their ranges.
/// Figures are separate source records so plain text never acquires image labels.
pub fn validate_structure(structure: &DocumentStructure, text_sources: &[StructureSource<'_>], figures: &[StructureSource<'_>]) -> Result<(), String> {
    if structure.version != 1 || structure.sections.len() > 10_000 || structure.blocks.len() > 100_000 ||
        serde_json::to_vec(structure).map_err(|e| e.to_string())?.len() > 8 * 1024 * 1024 {
        return fail("unsupported version or size limit");
    }
    if structure.blocks.is_empty() {
        return if structure.sections.is_empty() && text_sources.is_empty() && figures.is_empty() { Ok(()) }
            else { fail("empty structure has source content or sections") };
    }
    let mut identities = HashSet::new();
    let mut section_map = HashMap::new();
    for section in &structure.sections {
        if !valid_id(&section.id) || !identities.insert(section.id.as_str()) || ![1,2].contains(&section.columns) {
            return fail("duplicate section identity or invalid column count");
        }
        section_map.insert(section.id.as_str(), section);
    }
    let mut sources = HashMap::new();
    for source in text_sources.iter().chain(figures) {
        if !valid_id(source.id) || sources.insert(source.id, source).is_some() { return fail("duplicate or invalid source identity"); }
    }
    if structure.blocks.len() != sources.len() { return fail("every included source needs exactly one block"); }
    let mut source_ids = HashSet::new();
    let mut block_map = HashMap::new();
    let mut section_order: Vec<&str> = Vec::new();
    let mut section_last_column = HashMap::new();
    let mut total_runs = 0usize;
    let mut total_notes = 0usize;
    for block in &structure.blocks {
        if !valid_id(&block.id) || !identities.insert(block.id.as_str()) || !source_ids.insert(block.source_region_id.as_str()) {
            return fail("duplicate or invalid block/source identity");
        }
        let source = sources.get(block.source_region_id.as_str()).ok_or("Invalid document structure: source region is missing.")?;
        let section = section_map.get(block.section_id.as_str()).ok_or("Invalid document structure: section is missing.")?;
        if section_order.last().copied() != Some(block.section_id.as_str()) {
            if section_order.contains(&block.section_id.as_str()) || block.column != 1 { return fail("sections and reading columns must be contiguous"); }
            section_order.push(&block.section_id);
        }
        let last = section_last_column.entry(block.section_id.as_str()).or_insert(1);
        if block.column < *last || block.column < 1 || block.column > section.columns { return fail("invalid reading column order"); }
        *last = block.column;
        if !["start","center","end","justify"].contains(&block.alignment.as_str()) || !["none","page"].contains(&block.break_before.as_str()) {
            return fail("unsupported alignment or break");
        }
        let compatible = match source.kind {
            "picture" => block.role == "figure", "table" => block.role == "table", "formula" => block.role == "formula",
            "text"|"title"|"header"|"footer" => textual(&block.role), _ => false,
        };
        if !compatible { return fail("logical role does not match the source kind"); }
        if (block.role == "heading") != block.heading_level.is_some() || block.heading_level.is_some_and(|level| !(1..=6).contains(&level)) {
            return fail("heading level is missing or invalid");
        }
        if (block.role == "list-item") != block.list.is_some() { return fail("list properties do not match the role"); }
        if let Some(list) = &block.list {
            if !["bullet","number"].contains(&list.kind.as_str()) || list.level > 5 ||
                list.start.is_some_and(|start| list.kind != "number" || !(1..=1_000_000).contains(&start)) { return fail("invalid list properties"); }
        }
        if block.role != "caption" && block.anchor_block_id.is_some() { return fail("only captions may have a figure/table anchor"); }
        if block.role != "figure" && (block.alt_text.is_some() || block.decorative.is_some()) { return fail("only figures may have alternative text"); }
        if block.alt_text.as_ref().is_some_and(|text| text.encode_utf16().count() > 10_000 || text.contains('\0')) { return fail("figure alternative text exceeds the supported limit"); }
        let notes = block.note_references.as_deref().unwrap_or(&[]);
        if block.note_references.as_ref().is_some_and(Vec::is_empty) { return fail("empty note reference list must be omitted"); }
        if !textual(&block.role) && (!block.runs.is_empty() || block.note_references.is_some()) { return fail("text marks or note ranges on a nontext block"); }
        total_runs += block.runs.len(); total_notes += notes.len();
        if block.runs.len() > 10_000 || total_runs > 100_000 || notes.len() > 1_000 || total_notes > 10_000 { return fail("too many styled ranges or references"); }
        let edges = if block.runs.is_empty() && notes.is_empty() { HashMap::new() } else { boundaries(source.text) };
        let mut previous: Option<&InlineRun> = None;
        for run in &block.runs {
            if run.start >= run.end || !edges.contains_key(&run.start) || !edges.contains_key(&run.end) ||
                [run.bold,run.italic,run.underline].contains(&Some(false)) ||
                [run.bold,run.italic,run.underline].iter().all(Option::is_none) ||
                previous.is_some_and(|prior| prior.end > run.start || (prior.end == run.start && same_marks(prior,run))) {
                return fail("styled ranges must be ordered, canonical, and on complete characters");
            }
            previous = Some(run);
        }
        let mut last_end = 0;
        for reference in notes {
            if reference.start < last_end || reference.start >= reference.end || !edges.contains_key(&reference.start) || !edges.contains_key(&reference.end) ||
                !valid_id(&reference.target_block_id) || reference.target_block_id == block.id { return fail("invalid or overlapping note references"); }
            if source.text[edges[&reference.start]..edges[&reference.end]].trim().is_empty() { return fail("note reference marker is empty"); }
            last_end = reference.end;
        }
        block_map.insert(block.id.as_str(),block);
    }
    if section_order != structure.sections.iter().map(|section| section.id.as_str()).collect::<Vec<_>>() ||
        structure.sections.iter().any(|section| section_last_column.get(section.id.as_str()) != Some(&section.columns)) {
        return fail("empty section or incomplete two-column section");
    }
    if structure.blocks.iter().filter(|block| block.role != "figure").map(|block| block.source_region_id.as_str()).collect::<Vec<_>>() != text_sources.iter().map(|source| source.id).collect::<Vec<_>>() ||
        structure.blocks.iter().filter(|block| block.role == "figure").map(|block| block.source_region_id.as_str()).collect::<Vec<_>>() != figures.iter().map(|source| source.id).collect::<Vec<_>>() {
        return fail("logical and source reading order disagree");
    }
    for block in &structure.blocks {
        if let Some(anchor) = &block.anchor_block_id {
            if anchor == &block.id || !block_map.get(anchor.as_str()).is_some_and(|target| ["figure","table"].contains(&target.role.as_str())) { return fail("caption anchor is missing or incompatible"); }
        }
        for reference in block.note_references.iter().flatten() {
            if !block_map.get(reference.target_block_id.as_str()).is_some_and(|target| target.role == "footnote") { return fail("note target is missing or incompatible"); }
        }
    }
    // Iterative topological check avoids stack growth on adversarial note chains.
    let mut degree: HashMap<&str,usize> = block_map.keys().map(|id| (*id,0)).collect();
    for block in &structure.blocks { for reference in block.note_references.iter().flatten() { *degree.get_mut(reference.target_block_id.as_str()).unwrap() += 1; } }
    let mut ready: Vec<_> = degree.iter().filter_map(|(id,degree)| (*degree == 0).then_some(*id)).collect();
    let mut visited = 0;
    while let Some(id) = ready.pop() {
        visited += 1;
        for reference in block_map[id].note_references.iter().flatten() {
            let degree = degree.get_mut(reference.target_block_id.as_str()).unwrap(); *degree -= 1;
            if *degree == 0 { ready.push(reference.target_block_id.as_str()); }
        }
    }
    if visited != structure.blocks.len() { return fail("note references form a cycle"); }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn block(id:&str,source:&str,role:&str) -> serde_json::Value {
        json!({"id":id,"sourceRegionId":source,"sectionId":"section","column":1,"role":role,"alignment":"start","breakBefore":"none","runs":[]})
    }
    #[test]
    fn validates_literal_text_ranges_and_picture_note_links_without_geometry_guesses() {
        let mut heading=block("heading","r1","heading"); heading["headingLevel"]=json!(2);
        heading["runs"]=json!([{"start":3,"end":7,"bold":true}]);
        heading["noteReferences"]=json!([{"start":8,"end":9,"targetBlockId":"note"}]);
        let mut caption=block("caption","r3","caption"); caption["anchorBlockId"]=json!("figure");
        let mut figure=block("figure","r2","figure"); figure["altText"]=json!("A circuit");
        let value=json!({"version":1,"sections":[{"id":"section","columns":1}],"blocks":[heading,figure,caption,block("note","r4","footnote")]});
        let parsed:DocumentStructure=serde_json::from_value(value.clone()).unwrap();
        let text=[StructureSource{id:"r1",text:"😀 Title1",kind:"title"},StructureSource{id:"r3",text:"Figure",kind:"text"},StructureSource{id:"r4",text:"Reference",kind:"text"}];
        let figures=[StructureSource{id:"r2",text:"",kind:"picture"}];
        validate_structure(&parsed,&text,&figures).unwrap();
        assert_eq!(serde_json::to_value(&parsed).unwrap(),value);
        let mut invalid=parsed.clone(); invalid.blocks[0].runs[0].start=1;
        assert!(validate_structure(&invalid,&text,&figures).unwrap_err().contains("complete characters"));
        invalid=parsed.clone(); invalid.blocks[0].note_references.as_mut().unwrap()[0].target_block_id="missing".into();
        assert!(validate_structure(&invalid,&text,&figures).unwrap_err().contains("note target"));
        assert!(validate_structure(&parsed,&text,&[]).is_err());
    }
    #[test]
    fn rejects_interleaved_columns_unknown_versions_and_cyclic_notes() {
        let mut a=block("a","a","footnote"); a["noteReferences"]=json!([{"start":0,"end":1,"targetBlockId":"b"}]);
        let mut b=block("b","b","footnote"); b["noteReferences"]=json!([{"start":0,"end":1,"targetBlockId":"a"}]);
        let mut value=json!({"version":1,"sections":[{"id":"section","columns":1}],"blocks":[a,b,block("c","c","paragraph")]});
        let text=[StructureSource{id:"a",text:"1",kind:"text"},StructureSource{id:"b",text:"2",kind:"text"},StructureSource{id:"c",text:"3",kind:"text"}];
        let parse=|value| serde_json::from_value::<DocumentStructure>(value).unwrap();
        assert!(validate_structure(&parse(value.clone()),&text,&[]).unwrap_err().contains("cycle"));
        value["blocks"][0].as_object_mut().unwrap().remove("noteReferences");
        value["blocks"][1].as_object_mut().unwrap().remove("noteReferences");
        value["sections"][0]["columns"]=json!(2); value["blocks"][1]["column"]=json!(2);
        assert!(validate_structure(&parse(value.clone()),&text,&[]).unwrap_err().contains("column order"));
        value["blocks"][2]["column"]=json!(2);
        validate_structure(&parse(value.clone()),&text,&[]).unwrap();
        value["version"]=json!(2);
        assert!(validate_structure(&parse(value),&text,&[]).unwrap_err().contains("version"));
    }
}
